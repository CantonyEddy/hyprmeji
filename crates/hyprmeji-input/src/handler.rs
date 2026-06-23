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

    /// Mémorise la position curseur sans produire d'événement (hors drag).
    pub fn set_cursor(&mut self, pos: Vec2) {
        self.last_pos = Some(pos);
    }

    /// Bouton pressé : démarre un drag si le clic touche un pixel opaque.
    ///
    /// Retourne [`InputEvent::DragStart`] le cas échéant, `None` sinon.
    pub fn press<H: HitTest>(
        &mut self,
        cursor: Vec2,
        sprite_pos: Vec2,
        hit: &H,
    ) -> Option<InputEvent> {
        let local = cursor - sprite_pos;
        if !hit.is_opaque_at(local) {
            return None;
        }
        let grab_offset = cursor - sprite_pos;
        self.grab_offset = Some(grab_offset);
        self.history.clear();
        self.history.push_back(cursor);
        self.last_pos = Some(cursor);
        Some(InputEvent::DragStart { grab_offset })
    }

    /// Mouvement curseur : produit [`InputEvent::DragMove`] si un drag est actif.
    pub fn motion(&mut self, cursor: Vec2) -> Option<InputEvent> {
        self.last_pos = Some(cursor);
        if !self.is_dragging() {
            return None;
        }
        if self.history.len() == VELOCITY_WINDOW {
            self.history.pop_front();
        }
        self.history.push_back(cursor);
        Some(InputEvent::DragMove { cursor_pos: cursor })
    }

    /// Relâchement : termine le drag et émet [`InputEvent::DragEnd`].
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
/// délivrés par smithay-client-toolkit ; comme l'`InputHandler` vit désormais à
/// l'intérieur de l'état applicatif de `hyprmeji-render`, ce dernier relaie les
/// lots d'événements via [`InputHandler::ingest_events`]. Les [`InputEvent`]
/// résultants sont mis en file et récupérés via [`InputHandler::poll`].
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
    /// jamais. Le pointeur est souscrit via smithay-client-toolkit sur la file
    /// d'événements de l'appelant (paramètre générique `D`), ce qui permet à
    /// `hyprmeji-render` de le construire avec son propre `QueueHandle<AppState>`.
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

    /// Relaie un lot d'événements `wl_pointer` reçus par l'état hôte.
    ///
    /// Point d'entrée neutre vis-à-vis du type de file d'événements : il permet à
    /// `hyprmeji-render` de transférer les événements depuis son impl
    /// `PointerHandler for AppState` sans avoir à exposer un `QueueHandle`
    /// typé sur `InputHandler`.
    pub fn ingest_events(&mut self, events: &[PointerEvent]) {
        self.ingest(events);
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
