// crates/hyprmeji-render/src/surface.rs
//! Création et gestion de la layer surface `wlr-layer-shell`.
//!
//! La surface est de type **overlay**, transparente, sans interactivité clavier,
//! positionnée librement par ses marges (`top`/`left`). L'input region est vide
//! par défaut (passthrough total) puis mise à jour à chaque frame en
//! pixel-perfect : seuls les pixels d'alpha strictement positif sont saisissables.
//!
//! La logique de découpage de l'input region en rectangles (spans horizontaux de
//! pixels opaques par ligne) est **pure** et testée sans Wayland ; seule
//! l'application de ces rectangles à un `wl_region` touche au protocole.

use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::QueueHandle;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerSurface,
};
use smithay_client_toolkit::shell::WaylandSurface;

use crate::buffer::BYTES_PER_PIXEL;
use crate::error::RenderError;

/// Un rectangle d'input region (en coordonnées locales à la surface).
///
/// Tous les champs sont en pixels ; `w`/`h` sont strictement positifs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegionRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Extrait l'alpha (4ᵉ octet de chaque pixel) d'un buffer **RGBA**.
///
/// Renvoie un vecteur `width * height` d'octets d'alpha, row-major.
#[must_use]
pub(crate) fn alpha_channel_rgba(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let count = width as usize * height as usize;
    let mut out = Vec::with_capacity(count);
    let mut i = 3usize;
    while out.len() < count && i < rgba.len() {
        out.push(rgba[i]);
        i += BYTES_PER_PIXEL;
    }
    // Complète si le buffer est plus court qu'annoncé (cas défensif).
    while out.len() < count {
        out.push(0);
    }
    out
}

/// Construit les rectangles d'input region à partir d'un masque d'alpha.
///
/// Pour chaque ligne, regroupe les pixels d'alpha `> 0` en segments horizontaux
/// contigus, chacun produisant un rectangle de hauteur 1. C'est un découpage
/// simple et suffisant pour le hit-testing pixel-perfect : l'union des
/// rectangles couvre exactement les pixels opaques.
///
/// `alpha` doit contenir `width * height` octets (row-major). Les lignes
/// entièrement transparentes ne produisent aucun rectangle.
#[must_use]
pub(crate) fn input_region_rects(alpha: &[u8], width: u32, height: u32) -> Vec<RegionRect> {
    let w = width as usize;
    let h = height as usize;
    let mut rects = Vec::new();
    if w == 0 || h == 0 || alpha.len() < w * h {
        return rects;
    }
    for y in 0..h {
        let row = y * w;
        let mut x = 0usize;
        while x < w {
            if alpha[row + x] > 0 {
                let start = x;
                while x < w && alpha[row + x] > 0 {
                    x += 1;
                }
                rects.push(RegionRect {
                    x: start as i32,
                    y: y as i32,
                    w: (x - start) as i32,
                    h: 1,
                });
            } else {
                x += 1;
            }
        }
    }
    rects
}

/// Enveloppe la `LayerSurface` et mémorise sa taille courante.
pub(crate) struct Surface {
    layer: LayerSurface,
    width: u32,
    height: u32,
}

impl Surface {
    /// Crée une layer surface overlay transparente, ancrée en haut-gauche.
    ///
    /// La taille initiale est celle du premier sprite attendu ; elle est
    /// réajustée via [`Surface::set_size`] quand le sprite change. L'output est
    /// laissé au choix du compositor (moniteur principal en v1) en passant
    /// `None`.
    pub(crate) fn new<D>(
        layer_shell: &smithay_client_toolkit::shell::wlr_layer::LayerShell,
        qh: &QueueHandle<D>,
        wl_surface: WlSurface,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError>
    where
        D: smithay_client_toolkit::reexports::client::Dispatch<
                smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
                smithay_client_toolkit::shell::wlr_layer::LayerSurfaceData,
            > + 'static,
    {
        let layer = layer_shell.create_layer_surface(
            qh,
            wl_surface,
            Layer::Overlay,
            Some("hyprmeji"),
            None,
        );
        // Ancrage haut-gauche : la position absolue est portée par les marges.
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(width.max(1), height.max(1));
        // Passthrough total tant qu'aucune input region pixel-perfect n'est posée.
        layer.set_exclusive_zone(-1);
        layer.commit();

        Ok(Self {
            layer,
            width,
            height,
        })
    }

    /// Référence vers la `wl_surface` sous-jacente (pour `hyprmeji-input`).
    #[must_use]
    pub(crate) fn wl_surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    /// Met à jour la taille de la surface et ses marges de position.
    pub(crate) fn set_geometry(&mut self, width: u32, height: u32, margins: (i32, i32, i32, i32)) {
        if width != self.width || height != self.height {
            self.layer.set_size(width.max(1), height.max(1));
            self.width = width;
            self.height = height;
        }
        let (top, right, bottom, left) = margins;
        self.layer.set_margin(top, right, bottom, left);
    }

    /// Applique une input region pixel-perfect à la surface.
    ///
    /// Construit un `wl_region` à partir des rectangles fournis et l'installe
    /// comme input region. Une liste vide rend la surface entièrement
    /// passthrough. Le `wl_region` est créé via le `wl_compositor` et la file
    /// d'événements fournis par l'appelant.
    pub(crate) fn set_input_region<D>(
        &self,
        compositor: &smithay_client_toolkit::compositor::CompositorState,
        qh: &QueueHandle<D>,
        rects: &[RegionRect],
    ) -> Result<(), RenderError>
    where
        D: smithay_client_toolkit::reexports::client::Dispatch<
                smithay_client_toolkit::reexports::client::protocol::wl_region::WlRegion,
                (),
            > + 'static,
    {
        let region = compositor.wl_compositor().create_region(qh, ());
        for r in rects {
            region.add(r.x, r.y, r.w, r.h);
        }
        self.layer.wl_surface().set_input_region(Some(&region));
        region.destroy();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_channel_extracts_fourth_byte() {
        // 3 pixels RGBA, alpha = 0, 128, 255.
        let rgba = [1u8, 2, 3, 0, 4, 5, 6, 128, 7, 8, 9, 255];
        assert_eq!(alpha_channel_rgba(&rgba, 3, 1), vec![0, 128, 255]);
    }

    #[test]
    fn alpha_channel_pads_short_buffer() {
        let rgba = [1u8, 2, 3, 200]; // 1 pixel fourni, 3 attendus
        assert_eq!(alpha_channel_rgba(&rgba, 3, 1), vec![200, 0, 0]);
    }

    #[test]
    fn region_single_opaque_span() {
        // ligne 0 1 1 0 → un rectangle (1,0,2,1).
        let alpha = [0u8, 255, 255, 0];
        let rects = input_region_rects(&alpha, 4, 1);
        assert_eq!(
            rects,
            vec![RegionRect {
                x: 1,
                y: 0,
                w: 2,
                h: 1
            }]
        );
    }

    #[test]
    fn region_multiple_spans_same_row() {
        // 1 0 1 1 0 1 → spans (0,1) (2,2) (5,1).
        let alpha = [255u8, 0, 255, 255, 0, 255];
        let rects = input_region_rects(&alpha, 6, 1);
        assert_eq!(
            rects,
            vec![
                RegionRect { x: 0, y: 0, w: 1, h: 1 },
                RegionRect { x: 2, y: 0, w: 2, h: 1 },
                RegionRect { x: 5, y: 0, w: 1, h: 1 },
            ]
        );
    }

    #[test]
    fn region_spans_across_rows() {
        // 2x2 : ligne 0 = 1 1, ligne 1 = 0 1.
        let alpha = [255u8, 255, 0, 255];
        let rects = input_region_rects(&alpha, 2, 2);
        assert_eq!(
            rects,
            vec![
                RegionRect { x: 0, y: 0, w: 2, h: 1 },
                RegionRect { x: 1, y: 1, w: 1, h: 1 },
            ]
        );
    }

    #[test]
    fn region_fully_transparent_is_empty() {
        let alpha = [0u8; 9];
        assert!(input_region_rects(&alpha, 3, 3).is_empty());
    }

    #[test]
    fn region_fully_opaque_is_one_rect_per_row() {
        let alpha = [255u8; 6]; // 3x2
        let rects = input_region_rects(&alpha, 3, 2);
        assert_eq!(
            rects,
            vec![
                RegionRect { x: 0, y: 0, w: 3, h: 1 },
                RegionRect { x: 0, y: 1, w: 3, h: 1 },
            ]
        );
    }

    #[test]
    fn region_treats_any_nonzero_alpha_as_opaque() {
        let alpha = [1u8, 0, 254];
        let rects = input_region_rects(&alpha, 3, 1);
        assert_eq!(
            rects,
            vec![
                RegionRect { x: 0, y: 0, w: 1, h: 1 },
                RegionRect { x: 2, y: 0, w: 1, h: 1 },
            ]
        );
    }

    #[test]
    fn region_empty_dimensions_yield_nothing() {
        assert!(input_region_rects(&[], 0, 0).is_empty());
        assert!(input_region_rects(&[1, 2, 3], 0, 3).is_empty());
    }
}