// crates/hyprmeji-render/src/renderer.rs
//! `Renderer` : connexion Wayland, état applicatif SCTK et rendu des frames.
//!
//! Le `Renderer` détient la connexion, la file d'événements et l'état
//! applicatif ([`AppState`]) qui agrège les handlers smithay-client-toolkit
//! (registry, compositor, shm, layer-shell, output). Il expose l'API publique
//! [`Renderer::new`], [`Renderer::render_frame`] et [`Renderer::set_input_region`].
//!
//! Le rendu d'une frame consiste à : acquérir un buffer `wl_shm` libre (double
//! buffering), y copier les pixels de l'`AnimationFrame` après conversion
//! RGBA → ARGB8888 (et flip horizontal si `flip_x`), ajuster la géométrie de la
//! surface (taille + marges depuis la position), attacher le buffer, marquer la
//! zone endommagée et commit.

use hyprmeji_core::{AnimationFrame, Vec2};

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::delegate_compositor;
use smithay_client_toolkit::delegate_layer;
use smithay_client_toolkit::delegate_output;
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::delegate_shm;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_region::WlRegion;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{
    Connection, EventQueue, QueueHandle,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::shell::wlr_layer::{
    LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::{Shm, ShmHandler};

use crate::buffer::{
    flip_horizontal_rgba, margins_for, rgba_to_argb8888_into, BufferPool, BYTES_PER_PIXEL,
};
use crate::error::RenderError;
use crate::surface::{alpha_channel_rgba, input_region_rects, Surface};

/// État applicatif partagé avec les handlers smithay-client-toolkit.
struct AppState {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    shm: Shm,
    /// Surface du shimeji, créée à la construction.
    surface: Surface,
    /// `true` dès que le compositor a configuré la surface au moins une fois.
    configure_seen: bool,
}

impl AppState {
    fn wl_surface(&self) -> &WlSurface {
        self.surface.wl_surface()
    }
}

/// Renderer principal du crate.
///
/// Opaque : encapsule toute la machinerie Wayland. Voir les méthodes pour
/// l'API publique.
pub struct Renderer {
    /// Connexion Wayland. Conservée pour la durée de vie du renderer : sa
    /// libération fermerait la connexion au compositor. Jamais relue directement.
    #[allow(dead_code)]
    conn: Connection,
    event_queue: EventQueue<AppState>,
    qh: QueueHandle<AppState>,
    state: AppState,
    pool: BufferPool,
    /// Dimensions du dernier sprite rendu, pour détecter les changements.
    last_size: (u32, u32),
}

impl Renderer {
    /// Initialise la connexion Wayland et crée la layer surface overlay.
    ///
    /// Effectue les round-trips nécessaires pour récupérer les globals et la
    /// première configuration de la surface.
    ///
    /// # Erreurs
    /// - [`RenderError::Connect`] si la connexion au compositor échoue ;
    /// - [`RenderError::MissingGlobal`] si `wl_shm`, `wl_compositor` ou
    ///   `zwlr_layer_shell_v1` sont absents ;
    /// - [`RenderError::Dispatch`] en cas d'échec de round-trip.
    pub fn new() -> Result<Self, RenderError> {
        let conn =
            Connection::connect_to_env().map_err(|e| RenderError::Connect(e.to_string()))?;

        let (globals, mut event_queue) =
            registry_queue_init::<AppState>(&conn).map_err(|e| RenderError::Connect(e.to_string()))?;
        let qh = event_queue.handle();

        let registry_state = RegistryState::new(&globals);
        let output_state = OutputState::new(&globals, &qh);
        let compositor_state = CompositorState::bind(&globals, &qh)
            .map_err(|_| RenderError::MissingGlobal("wl_compositor"))?;
        let shm = Shm::bind(&globals, &qh).map_err(|_| RenderError::MissingGlobal("wl_shm"))?;
        let layer_shell = LayerShell::bind(&globals, &qh)
            .map_err(|_| RenderError::MissingGlobal("zwlr_layer_shell_v1"))?;

        // Taille initiale : 1x1, réajustée au premier `render_frame`.
        let init_w = 1;
        let init_h = 1;
        let wl_surface = compositor_state.create_surface(&qh);
        let surface = Surface::new(&layer_shell, &qh, wl_surface, init_w, init_h)?;

        let mut state = AppState {
            registry_state,
            output_state,
            compositor_state,
            shm,
            surface,
            configure_seen: false,
        };

        // Round-trip pour recevoir la configuration initiale de la surface.
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| RenderError::Dispatch(e.to_string()))?;

        let pool = BufferPool::new(&state.shm, init_w, init_h)?;

        Ok(Self {
            conn,
            event_queue,
            qh,
            state,
            pool,
            last_size: (init_w, init_h),
        })
    }

    /// Référence vers la `wl_surface` du shimeji (pour brancher `hyprmeji-input`).
    #[must_use]
    pub fn wl_surface(&self) -> &WlSurface {
        self.state.wl_surface()
    }

    /// Draine la file d'événements Wayland sans bloquer durablement.
    fn pump(&mut self) -> Result<(), RenderError> {
        self.event_queue
            .flush()
            .map_err(|e| RenderError::Dispatch(e.to_string()))?;
        if let Some(guard) = self.event_queue.prepare_read() {
            // Lecture non bloquante des éventuels événements en attente.
            let _ = guard.read();
        }
        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| RenderError::Dispatch(e.to_string()))?;
        Ok(())
    }

    /// Valide les dimensions d'une frame et la cohérence de son buffer.
    fn validate_frame(frame: &AnimationFrame) -> Result<(), RenderError> {
        if frame.width == 0 || frame.height == 0 {
            return Err(RenderError::InvalidDimensions {
                width: frame.width,
                height: frame.height,
            });
        }
        let expected = frame.width as usize * frame.height as usize * BYTES_PER_PIXEL;
        if frame.pixels.len() != expected {
            return Err(RenderError::BufferSizeMismatch {
                got: frame.pixels.len(),
                expected,
            });
        }
        Ok(())
    }

    /// Retourne les pixels RGBA effectifs de la frame, miroir appliqué si besoin.
    fn oriented_rgba(frame: &AnimationFrame) -> std::borrow::Cow<'_, [u8]> {
        if frame.flip_x {
            std::borrow::Cow::Owned(flip_horizontal_rgba(
                &frame.pixels,
                frame.width,
                frame.height,
            ))
        } else {
            std::borrow::Cow::Borrowed(&frame.pixels)
        }
    }

    /// Rasterise une frame à la position donnée et commit la surface.
    ///
    /// Convertit les pixels RGBA pré-décodés en ARGB8888 dans un buffer `wl_shm`
    /// libre (double buffering géré par le pool), applique le flip horizontal si
    /// `frame.flip_x`, positionne la surface via ses marges et commit.
    ///
    /// # Erreurs
    /// - [`RenderError::NotConfigured`] si la surface n'a pas encore été
    ///   configurée par le compositor ;
    /// - [`RenderError::InvalidDimensions`] / [`RenderError::BufferSizeMismatch`]
    ///   si la frame est incohérente ;
    /// - [`RenderError::BufferAlloc`] en cas d'échec d'allocation `wl_shm`.
    pub fn render_frame(
        &mut self,
        frame: &AnimationFrame,
        pos: Vec2,
    ) -> Result<(), RenderError> {
        self.pump()?;
        Self::validate_frame(frame)?;

        if !self.state.configure_seen {
            return Err(RenderError::NotConfigured);
        }

        let (w, h) = (frame.width, frame.height);

        // Ajuste le pool et la surface si la taille du sprite a changé.
        if (w, h) != self.last_size {
            self.pool.ensure_size(w, h)?;
            self.last_size = (w, h);
        }

        let oriented = Self::oriented_rgba(frame);

        // Acquiert un buffer libre et y écrit les pixels ARGB8888.
        // Le canvas emprunte le pool : on borne sa durée de vie au strict write.
        let buffer = {
            let (buffer, canvas) = self.pool.acquire(w, h)?;
            rgba_to_argb8888_into(&oriented, canvas);
            buffer
        };

        // Géométrie : taille du sprite + marges depuis la position.
        let margins = margins_for(pos.x, pos.y);
        self.state.surface.set_geometry(w, h, margins);

        // Attache, endommage toute la surface et commit.
        let wl_surface = self.state.surface.wl_surface().clone();
        buffer
            .attach_to(&wl_surface)
            .map_err(|e| RenderError::BufferAlloc(e.to_string()))?;
        wl_surface.damage_buffer(0, 0, w as i32, h as i32);
        wl_surface.commit();

        self.event_queue
            .flush()
            .map_err(|e| RenderError::Dispatch(e.to_string()))?;
        Ok(())
    }

    /// Met à jour l'input region (pixel-perfect hit testing).
    ///
    /// Construit la région à partir de l'alpha de la frame (après flip éventuel,
    /// pour rester aligné avec ce qui est affiché). Seuls les pixels d'alpha
    /// `> 0` deviennent saisissables ; le reste de la surface reste passthrough.
    ///
    /// # Erreurs
    /// - [`RenderError::InvalidDimensions`] / [`RenderError::BufferSizeMismatch`]
    ///   si la frame est incohérente ;
    /// - [`RenderError::Dispatch`] en cas d'échec de création de région.
    pub fn set_input_region(
        &mut self,
        frame: &AnimationFrame,
        _pos: Vec2,
    ) -> Result<(), RenderError> {
        Self::validate_frame(frame)?;

        let oriented = Self::oriented_rgba(frame);
        let alpha = alpha_channel_rgba(&oriented, frame.width, frame.height);
        let rects = input_region_rects(&alpha, frame.width, frame.height);

        self.state.surface.set_input_region(
            &self.state.compositor_state,
            &self.qh,
            &rects,
        )?;
        self.state.surface.wl_surface().commit();
        self.event_queue
            .flush()
            .map_err(|e| RenderError::Dispatch(e.to_string()))?;
        Ok(())
    }
}

// --- Implémentations des handlers smithay-client-toolkit ---

impl CompositorHandler for AppState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_transform: smithay_client_toolkit::reexports::client::protocol::wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _time: u32,
    ) {
    }
}

impl OutputHandler for AppState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }
}

impl ShmHandler for AppState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for AppState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {}

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.configure_seen = true;
    }
}

impl ProvidesRegistryState for AppState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

// Un `wl_region` ne porte aucun état utile pour nous : Dispatch trivial requis
// par `create_region(qh, ())`.
impl smithay_client_toolkit::reexports::client::Dispatch<WlRegion, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegion,
        _event: <WlRegion as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

delegate_compositor!(AppState);
delegate_output!(AppState);
delegate_shm!(AppState);
delegate_layer!(AppState);
delegate_registry!(AppState);

#[cfg(test)]
mod tests {
    use super::*;
    use hyprmeji_core::AnimationFrame;
    use std::sync::Arc;

    fn frame(w: u32, h: u32, flip_x: bool, fill: u8) -> AnimationFrame {
        let pixels = vec![fill; (w * h * 4) as usize];
        AnimationFrame {
            pixels: Arc::from(pixels.into_boxed_slice()),
            width: w,
            height: h,
            duration_ms: 100,
            flip_x,
        }
    }

    #[test]
    fn validate_rejects_zero_dimensions() {
        let f = AnimationFrame {
            pixels: Arc::from(Vec::new().into_boxed_slice()),
            width: 0,
            height: 4,
            duration_ms: 1,
            flip_x: false,
        };
        assert!(matches!(
            Renderer::validate_frame(&f),
            Err(RenderError::InvalidDimensions { .. })
        ));
    }

    #[test]
    fn validate_rejects_buffer_size_mismatch() {
        let f = AnimationFrame {
            pixels: Arc::from(vec![0u8; 8].into_boxed_slice()), // 2 px
            width: 4,
            height: 4, // attend 64 octets
            duration_ms: 1,
            flip_x: false,
        };
        assert!(matches!(
            Renderer::validate_frame(&f),
            Err(RenderError::BufferSizeMismatch {
                got: 8,
                expected: 64
            })
        ));
    }

    #[test]
    fn validate_accepts_consistent_frame() {
        let f = frame(2, 2, false, 255);
        assert!(Renderer::validate_frame(&f).is_ok());
    }

    #[test]
    fn oriented_rgba_borrows_when_not_flipped() {
        let f = frame(2, 1, false, 0);
        match Renderer::oriented_rgba(&f) {
            std::borrow::Cow::Borrowed(_) => {}
            std::borrow::Cow::Owned(_) => panic!("attendu un emprunt sans flip"),
        }
    }

    #[test]
    fn oriented_rgba_owns_and_flips_when_flagged() {
        // 2x1, deux pixels distincts pour observer l'inversion.
        let pixels = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
        let f = AnimationFrame {
            pixels: Arc::from(pixels.into_boxed_slice()),
            width: 2,
            height: 1,
            duration_ms: 1,
            flip_x: true,
        };
        match Renderer::oriented_rgba(&f) {
            std::borrow::Cow::Owned(v) => {
                assert_eq!(v, vec![50, 60, 70, 80, 10, 20, 30, 40]);
            }
            std::borrow::Cow::Borrowed(_) => panic!("attendu une copie avec flip"),
        }
    }
}