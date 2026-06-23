// crates/hyprmeji-render/src/renderer.rs
//! `Renderer` : connexion Wayland, état applicatif SCTK et rendu des frames.

use hyprmeji_core::{AnimationFrame, Vec2};

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::delegate_compositor;
use smithay_client_toolkit::delegate_layer;
use smithay_client_toolkit::delegate_output;
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::delegate_seat;
use smithay_client_toolkit::delegate_shm;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_region::WlRegion;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
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
    /// État du/des seat(s). Alimente la découverte du `wl_seat` et de ses
    /// capacités (le pointeur est souscrit côté `hyprmeji-input`).
    seat_state: SeatState,
    surface: Surface,
    configure_seen: bool,
    /// `wl_seat` retenu (le premier annoncé par le compositor), exposé au
    /// binaire pour construire un `InputHandler`. `None` tant qu'aucun seat n'a
    /// été annoncé.
    seat: Option<WlSeat>,
}

impl AppState {
    fn wl_surface(&self) -> &WlSurface {
        self.surface.wl_surface()
    }
}

pub struct Renderer {
    #[allow(dead_code)]
    conn: Connection,
    event_queue: EventQueue<AppState>,
    qh: QueueHandle<AppState>,
    state: AppState,
    pool: BufferPool,
    last_size: (u32, u32),
}

impl Renderer {
    pub fn new() -> Result<Self, RenderError> {
        let conn = Connection::connect_to_env().map_err(|e| RenderError::Connect(e.to_string()))?;

        let (globals, mut event_queue) = registry_queue_init::<AppState>(&conn)
            .map_err(|e| RenderError::Connect(e.to_string()))?;
        let qh = event_queue.handle();

        let registry_state = RegistryState::new(&globals);
        let output_state = OutputState::new(&globals, &qh);
        let seat_state = SeatState::new(&globals, &qh);
        let compositor_state = CompositorState::bind(&globals, &qh)
            .map_err(|_| RenderError::MissingGlobal("wl_compositor"))?;
        let shm = Shm::bind(&globals, &qh).map_err(|_| RenderError::MissingGlobal("wl_shm"))?;
        let layer_shell = LayerShell::bind(&globals, &qh)
            .map_err(|_| RenderError::MissingGlobal("zwlr_layer_shell_v1"))?;

        let init_w = 1;
        let init_h = 1;
        let wl_surface = compositor_state.create_surface(&qh);
        let surface = Surface::new(&layer_shell, &qh, wl_surface, init_w, init_h)?;

        let mut state = AppState {
            registry_state,
            output_state,
            compositor_state,
            shm,
            seat_state,
            surface,
            configure_seen: false,
            seat: None,
        };

        event_queue
            .roundtrip(&mut state)
            .map_err(|e| RenderError::Dispatch(e.to_string()))?;

        // Après le round-trip initial, le(s) seat(s) annoncé(s) par le
        // compositor sont connus. On retient le premier comme seat principal
        // (v1 : un seul seat géré). `new_seat` le renseigne aussi de façon
        // défensive si l'annonce arrive plus tard.
        if state.seat.is_none() {
            state.seat = state.seat_state.seats().next();
        }

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

    #[must_use]
    pub fn wl_surface(&self) -> &WlSurface {
        self.state.wl_surface()
    }

    /// Retourne le `WlSeat` pour construire un `InputHandler`.
    ///
    /// # Panics
    /// Panique si aucun `wl_seat` n'a été annoncé par le compositor au moment de
    /// l'initialisation. En pratique tout compositor Wayland expose au moins un
    /// seat ; l'absence relèverait d'un environnement dégradé.
    #[must_use]
    pub fn wl_seat(&self) -> &WlSeat {
        self.state
            .seat
            .as_ref()
            .expect("aucun wl_seat annoncé par le compositor")
    }

    /// Retourne les dimensions du moniteur principal `(width, height)`.
    ///
    /// Privilégie la taille logique de l'output (espace compositor), puis le
    /// mode courant, puis le premier mode disponible. En l'absence de tout
    /// output exploitable, retourne un repli `1920x1080`.
    #[must_use]
    pub fn screen_size(&self) -> (u32, u32) {
        const FALLBACK: (u32, u32) = (1920, 1080);

        let Some(output) = self.state.output_state.outputs().next() else {
            return FALLBACK;
        };
        let Some(info) = self.state.output_state.info(&output) else {
            return FALLBACK;
        };

        if let Some((w, h)) = info.logical_size {
            if w > 0 && h > 0 {
                return (w as u32, h as u32);
            }
        }

        let mode = info
            .modes
            .iter()
            .find(|m| m.current)
            .or_else(|| info.modes.first());
        if let Some(m) = mode {
            let (w, h) = m.dimensions;
            if w > 0 && h > 0 {
                return (w as u32, h as u32);
            }
        }

        FALLBACK
    }

    /// Pompe la file d'événements Wayland (non-bloquant, 1 dispatch).
    /// À appeler une fois par tick dans la boucle principale.
    ///
    /// # Erreurs
    /// - [`RenderError::Dispatch`] en cas d'échec de flush ou de dispatch.
    pub fn pump(&mut self) -> Result<(), RenderError> {
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

    pub fn render_frame(&mut self, frame: &AnimationFrame, pos: Vec2) -> Result<(), RenderError> {
        self.pump()?;
        Self::validate_frame(frame)?;

        if !self.state.configure_seen {
            return Err(RenderError::NotConfigured);
        }

        let (w, h) = (frame.width, frame.height);

        if (w, h) != self.last_size {
            self.pool.ensure_size(w, h)?;
            self.last_size = (w, h);
        }

        let oriented = Self::oriented_rgba(frame);

        let buffer = {
            let (buffer, canvas) = self.pool.acquire(w, h)?;
            rgba_to_argb8888_into(&oriented, canvas);
            buffer
        };

        let margins = margins_for(pos.x, pos.y);
        self.state.surface.set_geometry(w, h, margins);

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

    pub fn set_input_region(
        &mut self,
        frame: &AnimationFrame,
        _pos: Vec2,
    ) -> Result<(), RenderError> {
        Self::validate_frame(frame)?;

        let oriented = Self::oriented_rgba(frame);
        let alpha = alpha_channel_rgba(&oriented, frame.width, frame.height);
        let rects = input_region_rects(&alpha, frame.width, frame.height);

        self.state
            .surface
            .set_input_region(&self.state.compositor_state, &self.qh, &rects)?;
        self.state.surface.wl_surface().commit();
        self.event_queue
            .flush()
            .map_err(|e| RenderError::Dispatch(e.to_string()))?;
        Ok(())
    }
}

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

    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {
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

impl SeatHandler for AppState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: WlSeat) {
        // Retient le premier seat annoncé comme seat principal (v1 : mono-seat).
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: Capability,
    ) {
        // La souscription effective du pointeur est faite par `hyprmeji-input`
        // à partir du `WlSeat` exposé ; rien à faire ici.
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: Capability,
    ) {
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: WlSeat) {
        // Si le seat retenu disparaît, on l'oublie pour éviter d'exposer un
        // proxy mort.
        if self.seat.as_ref() == Some(&seat) {
            self.seat = None;
        }
    }
}

impl ProvidesRegistryState for AppState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

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
delegate_seat!(AppState);
delegate_shm!(AppState);
delegate_layer!(AppState);
delegate_registry!(AppState);
