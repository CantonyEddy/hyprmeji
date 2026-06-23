// crates/hyprmeji-core/src/animation.rs
//! Lecteur d'animations.
//!
//! Sélectionne l'animation correspondant à l'état courant et avance le timer de
//! frame en fonction du temps écoulé. Pas d'I/O : les frames sont fournies par
//! une [`SpriteSheet`] déjà décodée.

use crate::error::CoreError;
use crate::state::{Direction, State};
use crate::types::{AnimationFrame, SpriteSheet};

/// Lecteur d'animations adossé à une `SpriteSheet`.
#[derive(Debug, Clone)]
pub struct AnimationPlayer {
    sheet: SpriteSheet,
    current: String,
    frame_index: usize,
    elapsed_ms: u32,
}

impl AnimationPlayer {
    /// Crée un lecteur démarrant sur l'animation `initial`.
    ///
    /// # Erreurs
    /// Retourne [`CoreError::MissingAnimation`] si `initial` est absente de la
    /// feuille, ou [`CoreError::EmptyAnimation`] si elle ne contient aucune frame.
    pub fn new(sheet: SpriteSheet, initial: &str) -> Result<Self, CoreError> {
        Self::validate(&sheet, initial)?;
        Ok(Self {
            sheet,
            current: initial.to_string(),
            frame_index: 0,
            elapsed_ms: 0,
        })
    }

    fn validate(sheet: &SpriteSheet, name: &str) -> Result<(), CoreError> {
        match sheet.animation(name) {
            None => Err(CoreError::MissingAnimation {
                name: name.to_string(),
            }),
            Some(frames) if frames.is_empty() => Err(CoreError::EmptyAnimation {
                name: name.to_string(),
            }),
            Some(_) => Ok(()),
        }
    }

    /// Nom de l'animation jouée actuellement.
    #[must_use]
    pub fn current_animation(&self) -> &str {
        &self.current
    }

    /// Mappe un [`State`] vers le nom d'animation correspondant.
    #[must_use]
    pub fn animation_name_for(state: &State) -> &'static str {
        match state {
            State::Idle => "idle",
            State::Walk { .. } => "walk",
            State::ClimbWall { .. } => "climb",
            State::Fall { .. } => "fall",
            State::Land => "land",
            State::Dragged => "drag",
        }
    }

    /// Indique si l'animation courante doit être affichée en miroir.
    ///
    /// Pour `Walk { Left }`, on réutilise l'animation `walk` retournée (flip_x).
    #[must_use]
    pub fn flip_for(state: &State) -> bool {
        matches!(
            state,
            State::Walk {
                direction: Direction::Left
            }
        )
    }

    /// Synchronise le lecteur avec un nouvel état : change d'animation si besoin.
    ///
    /// Si l'animation demandée n'existe pas dans la feuille, le lecteur conserve
    /// l'animation courante et retourne `false` (pas d'erreur fatale — l'appelant
    /// peut décider de logguer). Retourne `true` si un changement a eu lieu.
    pub fn sync_to_state(&mut self, state: &State) -> bool {
        let target = Self::animation_name_for(state);
        if target == self.current {
            return false;
        }
        if Self::validate(&self.sheet, target).is_ok() {
            self.current = target.to_string();
            self.frame_index = 0;
            self.elapsed_ms = 0;
            true
        } else {
            false
        }
    }

    /// Avance le timer d'animation de `dt_ms` et retourne la frame courante.
    ///
    /// La lecture boucle sur l'animation. Le drapeau `flip_x` retourné est
    /// celui stocké dans la frame ; l'appelant peut le combiner avec
    /// [`AnimationPlayer::flip_for`] pour la direction.
    ///
    /// # Erreurs
    /// Retourne une erreur uniquement si l'animation courante a disparu ou est
    /// vide — situation rendue impossible par les validations de construction et
    /// de [`AnimationPlayer::sync_to_state`], mais traitée sans panique ni
    /// `unwrap()` par prudence.
    pub fn advance(&mut self, dt_ms: u32) -> Result<AnimationFrame, CoreError> {
        let frames = match self.sheet.animation(&self.current) {
            Some(f) if !f.is_empty() => f,
            Some(_) => {
                return Err(CoreError::EmptyAnimation {
                    name: self.current.clone(),
                })
            }
            None => {
                return Err(CoreError::MissingAnimation {
                    name: self.current.clone(),
                })
            }
        };

        self.elapsed_ms = self.elapsed_ms.saturating_add(dt_ms);

        // Avance d'autant de frames que la durée écoulée le permet (boucle).
        // Si l'index est hors borne (animation raccourcie entre-temps), on le
        // ramène dans l'intervalle sans paniquer.
        if self.frame_index >= frames.len() {
            self.frame_index = 0;
            self.elapsed_ms = 0;
        }
        loop {
            let dur = frames[self.frame_index].duration_ms.max(1);
            if self.elapsed_ms < dur {
                break;
            }
            self.elapsed_ms -= dur;
            self.frame_index = (self.frame_index + 1) % frames.len();
        }

        Ok(frames[self.frame_index].clone())
    }

    /// Indique si l'animation courante vient de boucler (frame revenue à 0).
    #[must_use]
    pub fn is_at_first_frame(&self) -> bool {
        self.frame_index == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Direction;
    use crate::types::AnimationFrame;
    use std::sync::Arc;

    fn frame(duration_ms: u32) -> AnimationFrame {
        AnimationFrame {
            pixels: Arc::from(vec![0u8; 4].into_boxed_slice()),
            width: 1,
            height: 1,
            duration_ms,
            flip_x: false,
        }
    }

    fn sheet_with(names: &[(&str, usize)]) -> SpriteSheet {
        let mut s = SpriteSheet::new();
        for (name, count) in names {
            let frames: Vec<_> = (0..*count).map(|_| frame(100)).collect();
            s.animations.insert((*name).to_string(), frames);
        }
        s
    }

    #[test]
    fn new_fails_on_missing_animation() {
        let s = sheet_with(&[("idle", 1)]);
        let err = AnimationPlayer::new(s, "walk").unwrap_err();
        assert!(matches!(err, CoreError::MissingAnimation { .. }));
    }

    #[test]
    fn new_fails_on_empty_animation() {
        let s = sheet_with(&[("idle", 0)]);
        let err = AnimationPlayer::new(s, "idle").unwrap_err();
        assert!(matches!(err, CoreError::EmptyAnimation { .. }));
    }

    #[test]
    fn advance_loops_frames() {
        let s = sheet_with(&[("idle", 2)]);
        let mut p = AnimationPlayer::new(s, "idle").expect("idle exists");
        assert!(p.is_at_first_frame());
        // 100ms : passe à la frame 1.
        p.advance(100).expect("frame available");
        assert!(!p.is_at_first_frame());
        // 100ms de plus : boucle vers la frame 0.
        p.advance(100).expect("frame available");
        assert!(p.is_at_first_frame());
    }

    #[test]
    fn advance_handles_large_dt() {
        let s = sheet_with(&[("idle", 3)]);
        let mut p = AnimationPlayer::new(s, "idle").expect("idle exists");
        // 100ms * 3 frames = un cycle complet → revient à la frame 0.
        p.advance(300).expect("frame available");
        assert!(p.is_at_first_frame());
    }

    #[test]
    fn animation_name_mapping() {
        assert_eq!(AnimationPlayer::animation_name_for(&State::Idle), "idle");
        assert_eq!(
            AnimationPlayer::animation_name_for(&State::Walk {
                direction: Direction::Right
            }),
            "walk"
        );
        assert_eq!(AnimationPlayer::animation_name_for(&State::Land), "land");
        assert_eq!(AnimationPlayer::animation_name_for(&State::Dragged), "drag");
    }

    #[test]
    fn flip_for_walk_left() {
        assert!(AnimationPlayer::flip_for(&State::Walk {
            direction: Direction::Left
        }));
        assert!(!AnimationPlayer::flip_for(&State::Walk {
            direction: Direction::Right
        }));
        assert!(!AnimationPlayer::flip_for(&State::Idle));
    }

    #[test]
    fn sync_changes_animation_when_present() {
        let s = sheet_with(&[("idle", 1), ("walk", 2)]);
        let mut p = AnimationPlayer::new(s, "idle").expect("idle exists");
        let changed = p.sync_to_state(&State::Walk {
            direction: Direction::Right,
        });
        assert!(changed);
        assert_eq!(p.current_animation(), "walk");
    }

    #[test]
    fn sync_keeps_animation_when_missing() {
        let s = sheet_with(&[("idle", 1)]);
        let mut p = AnimationPlayer::new(s, "idle").expect("idle exists");
        // "walk" absente : pas de changement, pas de panique.
        let changed = p.sync_to_state(&State::Walk {
            direction: Direction::Right,
        });
        assert!(!changed);
        assert_eq!(p.current_animation(), "idle");
    }

    #[test]
    fn sync_same_state_is_noop() {
        let s = sheet_with(&[("idle", 1)]);
        let mut p = AnimationPlayer::new(s, "idle").expect("idle exists");
        assert!(!p.sync_to_state(&State::Idle));
    }
}