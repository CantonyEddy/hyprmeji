// crates/hyprmeji-input/src/handler.rs
//! `InputHandler` (adaptateur `wl_pointer`) et `DragTracker` (logique pure).
//!
//! La logique métier du drag est isolée dans [`DragTracker`], totalement
//! indépendante de Wayland et testable en isolation. [`InputHandler`] se contente
//! de traduire les événements `wl_pointer` reçus de smithay-client-toolkit en
//! appels au tracker, puis d'exposer les [`InputEvent`] produits via
//! [`InputHandler::poll`].

use std::collections::VecDeque;

use hyprmeji_core::Vec2;

use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::seat::pointer::{
    PointerData, PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT,
};

use crate::error::InputError;

/// Nombre de positions curseur conservées pour le calcul de vélocité.
const VELOCITY_WINDOW: usize = 5;
/// Facteur de conversion delta/frame → px/s en supposant 60 fps.
const FPS: f32 = 60.0;

/// Événements d'entrée émis vers la boucle principale.
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// Début de drag : bouton gauche pressé sur un pixel opaque du sprite.
    ///
    /// `grab_offset` = position curseur − position sprite (point de saisie).
    DragStart { grab_offset: Vec2 },
    /// Déplacement pendant le drag : position absolue du curseur.
    DragMove { cursor_pos: Vec2 },
    /// Fin de drag : vélocité moyenne du curseur (px/s).
    DragEnd { cursor_vel: Vec2 },
}

/// Test d'opacité pixel-perfect du sprite courant.
///
/// La boucle principale connaît la frame et la position du sprite ; on lui
/// délègue donc la décision « ce pixel local est-il opaque ? ». Le tracker reste
/// ainsi pur et indépendant du contenu des sprites.
pub trait HitTest {
    /// Vrai si le pixel local `(x, y)` (relatif au coin haut-gauche du sprite)
    /// est non transparent et donc saisissable.
    fn is_opaque_at(&self, local: Vec2) -> bool;
}

/// Machine d'états *pure* du drag.
///
/// Indépendante de Wayland : on l'alimente avec des positions curseur absolues
/// et des transitions de bouton, elle produit des [`InputEvent`]. Toute la
/// logique testable (vélocité, grab_offset, idle ↔ dragging) vit ici.
#[derive(Debug, Default)]
pub struct DragTracker {
    /// `Some(grab_offset)` lorsqu'un drag est en cours.
    grab_offset: Option<Vec2>,
    /// Historique borné des dernières positions curseur (pour la vélocité).
    history: VecDeque<Vec2>,
    /// Dernière position curseur connue.
    last_pos: Option<Vec2>,
}

impl DragTracker {
    /// Crée un tracker au repos.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Indique si un drag est actuellement en cours.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.grab_offset.is_some()
    }

    /// Met à jour la dernière position curseur connue (sans transition).
    pub fn set_cursor(&mut self, pos: Vec2) {
        self.last_pos = Some(pos);
    }

    /// Bouton gauche pressé à `cursor_pos`, sprite ancré en `sprite_pos`.
    ///
    /// Démarre un drag **uniquement** si `hit.is_opaque_at` accepte le pixel
    /// local sous le curseur. Retourne l'[`InputEvent::DragStart`] le cas
    /// échéant, sinon `None` (clic dans une zone transparente ou hors sprite).
    pub fn press(
        &mut self,
        cursor_pos: Vec2,
        sprite_pos: Vec2,
        hit: &impl HitTest,
    ) -> Option<InputEvent> {
        self.last_pos = Some(cursor_pos);
        if self.is_dragging() {
            return None;
        }
        let local = cursor_pos - sprite_pos;
        if !hit.is_opaque_at(local) {
            return None;
        }
        let grab_offset = local;
        self.grab_offset = Some(grab_offset);
        self.history.clear();
        self.history.push_back(cursor_pos);
        Some(InputEvent::DragStart { grab_offset })
    }

    /// Curseur déplacé à `cursor_pos`.
    ///
    /// Émet [`InputEvent::DragMove`] si un drag est en cours, en alimentant
    /// l'historique de vélocité. Hors drag, met seulement à jour la dernière
    /// position et retourne `None`.
    pub fn motion(&mut self, cursor_pos: Vec2) -> Option<InputEvent> {
        self.last_pos = Some(cursor_pos);
        if !self.is_dragging() {
            return None;
        }
        self.history.push_back(cursor_pos);
        while self.history.len() > VELOCITY_WINDOW {
            self.history.pop_front();
        }
        Some(InputEvent::DragMove { cursor_pos })
    }

    /// Bouton gauche relâché.
    ///
    /// Termine le drag et émet [`InputEvent::DragEnd`] avec la vélocité moyenne
    /// calculée sur l'historique. Hors drag, retourne `None`.
    pub fn release(&mut self) -> Option<InputEvent> {
        if !self.is_dragging() {
            return None;
        }
        let cursor_vel = self.average_velocity();
        self.grab_offset = None;
        self.history.clear();
        Some(InputEvent::DragEnd { cursor_vel })
    }

    /// Vélocité moyenne (px/s) à partir des deltas entre positions consécutives.
    ///
    /// Moyenne des deltas des dernières frames × `FPS`. Retourne `Vec2::ZERO`
    /// s'il y a moins de deux positions (aucun mouvement mesurable).
    #[must_use]
    fn average_velocity(&self) -> Vec2 {
        if self.history.len() < 2 {
            return Vec2::ZERO;
        }
        let mut sum = Vec2::ZERO;
        let mut prev: Option<Vec2> = None;
        let mut count = 0.0_f32;
        for &p in &self.history {
            if let Some(q) = prev {
                sum += p - q;
                count += 1.0;
            }
            prev = Some(p);
        }
        if count == 0.0 {
            return Vec2::ZERO;
        }
        (sum * (1.0 / count)) * FPS
    }
}

/// Adaptateur `wl_pointer` : traduit les événements Wayland vers le tracker.
///
/// Construit via [`InputHandler::new`] à partir d'un `WlSeat` (dont la surface
/// cible est créée ailleurs par `hyprmeji-render`). Les événements pointeur sont
/// délivrés par smithay-client-toolkit au travers de l'impl [`PointerHandler`] ;
/// les [`InputEvent`] résultants sont mis en file et récupérés via
/// [`InputHandler::poll`].
pub struct InputHandler {
    /// Le pointeur Wayland souscrit sur le seat.
    pointer: WlPointer,
    /// Surface cible (celle du shimeji) ; les événements hors d'elle sont ignorés.
    surface: WlSurface,
    /// Logique pure du drag.
    tracker: DragTracker,
    /// File d'événements produits, vidée par `poll`.
    pending: VecDeque<InputEvent>,
    /// Position courante du sprite, mise à jour par la boucle principale.
    sprite_pos: Vec2,
    /// Hit-test opaque courant fourni par la boucle principale.
    hit: OpaqueMask,
}

/// Masque d'opacité courant, alimenté par la boucle principale à chaque frame.
///
/// Par défaut (`OpaqueMask::default`), aucun pixel n'est saisissable tant que la
/// boucle principale n'a pas fourni de masque ; elle appelle
/// [`InputHandler::set_hit_mask`] avec un masque rectangulaire ou pixel-perfect.
#[derive(Debug, Clone, Default)]
pub struct OpaqueMask {
    /// Largeur/hauteur du sprite ; un clic dans ce rectangle est saisissable.
    size: Option<(f32, f32)>,
    /// Pixels d'alpha (`width * height`), `Some` si masque pixel-perfect fourni.
    alpha: Option<(u32, u32, Vec<u8>)>,
}

impl OpaqueMask {
    /// Masque permissif : tout pixel dans `size` est saisissable.
    #[must_use]
    pub fn rect(width: f32, height: f32) -> Self {
        Self {
            size: Some((width, height)),
            alpha: None,
        }
    }

    /// Masque pixel-perfect à partir d'un buffer d'alpha row-major.
    #[must_use]
    pub fn pixels(width: u32, height: u32, alpha: Vec<u8>) -> Self {
        Self {
            size: Some((width as f32, height as f32)),
            alpha: Some((width, height, alpha)),
        }
    }
}

impl HitTest for OpaqueMask {
    fn is_opaque_at(&self, local: Vec2) -> bool {
        let Some((w, h)) = self.size else {
            return false;
        };
        if local.x < 0.0 || local.y < 0.0 || local.x >= w || local.y >= h {
            return false;
        }
        match &self.alpha {
            None => true,
            Some((aw, ah, buf)) => {
                let px = local.x as u32;
                let py = local.y as u32;
                if px >= *aw || py >= *ah {
                    return false;
                }
                let idx = (py * *aw + px) as usize;
                buf.get(idx).is_some_and(|&a| a > 0)
            }
        }
    }
}

impl InputHandler {
    /// Crée un handler attaché au pointeur d'un `WlSeat` existant.
    ///
    /// La `surface` est celle créée par `hyprmeji-render` ; ce crate ne la crée
    /// jamais. Le pointeur est souscrit via smithay-client-toolkit.
    ///
    /// # Erreurs
    /// Retourne [`InputError::PointerInit`] si l'obtention du pointeur échoue.
    /// (La capacité pointeur du seat est supposée vérifiée par l'appelant ;
    /// [`InputError::NoPointer`] est disponible pour signaler son absence.)
    pub fn new<D>(
        seat: &WlSeat,
        surface: WlSurface,
        qh: &QueueHandle<D>,
    ) -> Result<Self, InputError>
    where
        D: Dispatch<WlPointer, PointerData> + 'static,
    {
        let pointer = seat.get_pointer(qh, PointerData::new(seat.clone()));

        Ok(Self {
            pointer,
            surface,
            tracker: DragTracker::new(),
            pending: VecDeque::new(),
            sprite_pos: Vec2::ZERO,
            hit: OpaqueMask::default(),
        })
    }

    /// Référence vers le `wl_pointer` souscrit (utile à l'intégration SCTK).
    #[must_use]
    pub fn pointer(&self) -> &WlPointer {
        &self.pointer
    }

    /// Met à jour la position du sprite (appelé par la boucle principale).
    pub fn set_sprite_pos(&mut self, pos: Vec2) {
        self.sprite_pos = pos;
    }

    /// Met à jour le masque d'opacité courant (pixel-perfect hit testing).
    pub fn set_hit_mask(&mut self, mask: OpaqueMask) {
        self.hit = mask;
    }

    /// Retourne le prochain événement en attente, non bloquant.
    pub fn poll(&mut self) -> Option<InputEvent> {
        self.pending.pop_front()
    }

    /// Traduit un lot d'événements `wl_pointer` en [`InputEvent`].
    ///
    /// Séparé de l'impl `PointerHandler` pour être réutilisable et lisible. Les
    /// événements ne concernant pas la surface cible sont ignorés.
    fn ingest(&mut self, events: &[PointerEvent]) {
        for ev in events {
            if ev.surface != self.surface {
                continue;
            }
            let pos = Vec2::new(ev.position.0 as f32, ev.position.1 as f32);
            match ev.kind {
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    if let Some(e) = self.tracker.press(pos, self.sprite_pos, &self.hit) {
                        self.pending.push_back(e);
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    if let Some(e) = self.tracker.release() {
                        self.pending.push_back(e);
                    }
                }
                PointerEventKind::Motion { .. } => {
                    if let Some(e) = self.tracker.motion(pos) {
                        self.pending.push_back(e);
                    } else {
                        self.tracker.set_cursor(pos);
                    }
                }
                PointerEventKind::Leave { .. } => {
                    // Curseur quitte la surface : on termine un éventuel drag.
                    if let Some(e) = self.tracker.release() {
                        self.pending.push_back(e);
                    }
                }
                _ => {}
            }
        }
    }
}

impl PointerHandler for InputHandler {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        self.ingest(events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hit-test de test : accepte tout point dans `[0, size)`.
    struct RectHit {
        w: f32,
        h: f32,
    }
    impl HitTest for RectHit {
        fn is_opaque_at(&self, local: Vec2) -> bool {
            local.x >= 0.0 && local.y >= 0.0 && local.x < self.w && local.y < self.h
        }
    }

    fn rect() -> RectHit {
        RectHit { w: 128.0, h: 128.0 }
    }

    fn approx(a: Vec2, b: Vec2) {
        assert!((a.x - b.x).abs() < 0.001, "x: {} vs {}", a.x, b.x);
        assert!((a.y - b.y).abs() < 0.001, "y: {} vs {}", a.y, b.y);
    }

    #[test]
    fn press_on_opaque_starts_drag_with_grab_offset() {
        let mut t = DragTracker::new();
        // Sprite ancré en (100,100), clic en (130,150) → offset (30,50).
        let ev = t
            .press(Vec2::new(130.0, 150.0), Vec2::new(100.0, 100.0), &rect())
            .expect("drag start");
        match ev {
            InputEvent::DragStart { grab_offset } => approx(grab_offset, Vec2::new(30.0, 50.0)),
            other => panic!("attendu DragStart, obtenu {other:?}"),
        }
        assert!(t.is_dragging());
    }

    #[test]
    fn press_outside_sprite_does_not_start_drag() {
        let mut t = DragTracker::new();
        // Clic à gauche du sprite → local négatif → rejeté.
        let ev = t.press(Vec2::new(10.0, 10.0), Vec2::new(100.0, 100.0), &rect());
        assert!(ev.is_none());
        assert!(!t.is_dragging());
    }

    #[test]
    fn press_on_transparent_pixel_is_rejected() {
        // Hit-test qui refuse tout.
        struct Never;
        impl HitTest for Never {
            fn is_opaque_at(&self, _: Vec2) -> bool {
                false
            }
        }
        let mut t = DragTracker::new();
        let ev = t.press(Vec2::new(110.0, 110.0), Vec2::new(100.0, 100.0), &Never);
        assert!(ev.is_none());
        assert!(!t.is_dragging());
    }

    #[test]
    fn double_press_is_noop_while_dragging() {
        let mut t = DragTracker::new();
        t.press(Vec2::new(110.0, 110.0), Vec2::new(100.0, 100.0), &rect());
        let second = t.press(Vec2::new(120.0, 120.0), Vec2::new(100.0, 100.0), &rect());
        assert!(second.is_none());
        assert!(t.is_dragging());
    }

    #[test]
    fn motion_without_drag_returns_none() {
        let mut t = DragTracker::new();
        assert!(t.motion(Vec2::new(5.0, 5.0)).is_none());
        assert!(!t.is_dragging());
    }

    #[test]
    fn motion_during_drag_emits_drag_move() {
        let mut t = DragTracker::new();
        t.press(Vec2::new(110.0, 110.0), Vec2::new(100.0, 100.0), &rect());
        let ev = t.motion(Vec2::new(115.0, 112.0)).expect("drag move");
        match ev {
            InputEvent::DragMove { cursor_pos } => approx(cursor_pos, Vec2::new(115.0, 112.0)),
            other => panic!("attendu DragMove, obtenu {other:?}"),
        }
    }

    #[test]
    fn release_without_drag_returns_none() {
        let mut t = DragTracker::new();
        assert!(t.release().is_none());
    }

    #[test]
    fn full_cycle_idle_dragging_idle() {
        let mut t = DragTracker::new();
        assert!(!t.is_dragging());
        t.press(Vec2::new(110.0, 110.0), Vec2::new(100.0, 100.0), &rect());
        assert!(t.is_dragging());
        t.motion(Vec2::new(120.0, 110.0));
        let end = t.release().expect("drag end");
        assert!(matches!(end, InputEvent::DragEnd { .. }));
        assert!(!t.is_dragging());
    }

    #[test]
    fn average_velocity_constant_step() {
        let mut t = DragTracker::new();
        t.press(Vec2::new(0.0, 0.0), Vec2::ZERO, &rect());
        // 4 mouvements de +10px en x → delta moyen 10 → ×60 = 600 px/s.
        for i in 1..=4 {
            t.motion(Vec2::new(10.0 * i as f32, 0.0));
        }
        match t.release().expect("end") {
            InputEvent::DragEnd { cursor_vel } => approx(cursor_vel, Vec2::new(600.0, 0.0)),
            other => panic!("attendu DragEnd, obtenu {other:?}"),
        }
    }

    #[test]
    fn average_velocity_diagonal() {
        let mut t = DragTracker::new();
        t.press(Vec2::new(0.0, 0.0), Vec2::ZERO, &rect());
        // Deltas (5, -2) répétés → vélocité (300, -120).
        for i in 1..=3 {
            t.motion(Vec2::new(5.0 * i as f32, -2.0 * i as f32));
        }
        match t.release().expect("end") {
            InputEvent::DragEnd { cursor_vel } => approx(cursor_vel, Vec2::new(300.0, -120.0)),
            other => panic!("attendu DragEnd, obtenu {other:?}"),
        }
    }

    #[test]
    fn velocity_window_is_bounded_to_last_five() {
        let mut t = DragTracker::new();
        t.press(Vec2::new(0.0, 0.0), Vec2::ZERO, &rect());
        // Beaucoup de petits pas (1px), puis on vérifie que seuls les 5 derniers
        // (4 deltas de 1px) comptent → moyenne 1 → ×60 = 60 px/s.
        for i in 1..=20 {
            t.motion(Vec2::new(i as f32, 0.0));
        }
        match t.release().expect("end") {
            InputEvent::DragEnd { cursor_vel } => approx(cursor_vel, Vec2::new(60.0, 0.0)),
            other => panic!("attendu DragEnd, obtenu {other:?}"),
        }
    }

    #[test]
    fn velocity_zero_without_motion() {
        let mut t = DragTracker::new();
        t.press(Vec2::new(50.0, 50.0), Vec2::ZERO, &rect());
        // Aucun mouvement : historique = 1 position → vélocité nulle.
        match t.release().expect("end") {
            InputEvent::DragEnd { cursor_vel } => approx(cursor_vel, Vec2::ZERO),
            other => panic!("attendu DragEnd, obtenu {other:?}"),
        }
    }

    #[test]
    fn opaque_mask_rect_accepts_inside_rejects_outside() {
        let m = OpaqueMask::rect(10.0, 10.0);
        assert!(m.is_opaque_at(Vec2::new(0.0, 0.0)));
        assert!(m.is_opaque_at(Vec2::new(9.0, 9.0)));
        assert!(!m.is_opaque_at(Vec2::new(10.0, 5.0)));
        assert!(!m.is_opaque_at(Vec2::new(-1.0, 5.0)));
    }

    #[test]
    fn opaque_mask_pixels_respects_alpha() {
        // 2x1 : pixel (0,0) transparent, (1,0) opaque.
        let m = OpaqueMask::pixels(2, 1, vec![0, 255]);
        assert!(!m.is_opaque_at(Vec2::new(0.0, 0.0)));
        assert!(m.is_opaque_at(Vec2::new(1.0, 0.0)));
    }
}