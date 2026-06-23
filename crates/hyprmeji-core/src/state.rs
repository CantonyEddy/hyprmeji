// crates/hyprmeji-core/src/state.rs
//! Machine d'états du shimeji.
//!
//! La fonction [`transition`] est **pure** : elle ne lit ni n'écrit aucun état
//! global, ne fait pas d'I/O, et retourne le nouvel état (ou `None` si l'état
//! ne change pas) en fonction de l'état courant et de l'événement reçu.

use crate::types::Vec2;

/// État courant du shimeji.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Immobile, animation `idle` en boucle.
    Idle,
    /// Marche horizontale dans une direction.
    Walk { direction: Direction },
    /// Grimpe le bord d'une fenêtre.
    ClimbWall { window_id: u32, side: WallSide },
    /// En chute libre.
    Fall { velocity: Vec2 },
    /// Animation d'atterrissage.
    Land,
    /// Saisi par le curseur (physique suspendue).
    Dragged,
}

/// Direction de déplacement horizontal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
}

impl Direction {
    /// Retourne la direction opposée.
    #[must_use]
    pub fn opposite(self) -> Direction {
        match self {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
        }
    }
}

/// Côté d'une fenêtre auquel le shimeji s'accroche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallSide {
    Left,
    Right,
}

/// Événements pouvant déclencher une transition.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Tick d'horloge (chaque frame), `dt_ms` millisecondes écoulées.
    Tick { dt_ms: u32 },
    /// Un bord (d'écran ou de fenêtre) a été atteint.
    ReachedEdge,
    /// Une fenêtre est à proximité immédiate.
    WindowNearby {
        id: u32,
        side: WallSide,
        x: f32,
        y: f32,
    },
    /// Une fenêtre suivie a disparu.
    WindowGone { id: u32 },
    /// Début de drag souris.
    DragStart,
    /// Fin de drag, avec la vélocité au moment du relâcher.
    DragEnd { velocity: Vec2 },
    /// L'animation d'atterrissage est terminée.
    LandingAnimDone,
    /// Le timer idle a expiré.
    IdleTimerFired,
}

/// Fonction de transition **pure** de la machine d'états.
///
/// Retourne `Some(nouvel_état)` si l'événement provoque un changement, sinon
/// `None` (l'appelant conserve alors l'état courant).
#[must_use]
pub fn transition(state: &State, event: &Event) -> Option<State> {
    match (state, event) {
        // --- Le drag a priorité depuis presque tous les états ---
        (s, Event::DragStart) if *s != State::Dragged => Some(State::Dragged),

        // --- Depuis Idle ---
        (State::Idle, Event::IdleTimerFired) => Some(State::Walk {
            direction: Direction::Right,
        }),
        (State::Idle, Event::WindowNearby { id, side, .. }) => Some(State::ClimbWall {
            window_id: *id,
            side: *side,
        }),

        // --- Depuis Walk ---
        (State::Walk { .. }, Event::WindowNearby { id, side, .. }) => Some(State::ClimbWall {
            window_id: *id,
            side: *side,
        }),
        (State::Walk { direction }, Event::ReachedEdge) => Some(State::Walk {
            direction: direction.opposite(),
        }),

        // --- Depuis ClimbWall ---
        (State::ClimbWall { window_id, .. }, Event::WindowGone { id }) if window_id == id => {
            Some(State::Fall {
                velocity: Vec2::ZERO,
            })
        }
        (State::ClimbWall { .. }, Event::ReachedEdge) => Some(State::Idle),

        // --- Depuis Fall ---
        (State::Fall { .. }, Event::ReachedEdge) => Some(State::Land),

        // --- Depuis Land ---
        (State::Land, Event::LandingAnimDone) => Some(State::Idle),

        // --- Depuis Dragged ---
        (State::Dragged, Event::DragEnd { velocity }) => Some(State::Fall {
            velocity: *velocity,
        }),

        // --- Aucun changement ---
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_opposite() {
        assert_eq!(Direction::Left.opposite(), Direction::Right);
        assert_eq!(Direction::Right.opposite(), Direction::Left);
    }

    #[test]
    fn idle_timer_starts_walk() {
        let next = transition(&State::Idle, &Event::IdleTimerFired);
        assert_eq!(
            next,
            Some(State::Walk {
                direction: Direction::Right
            })
        );
    }

    #[test]
    fn walk_reverses_at_edge() {
        let s = State::Walk {
            direction: Direction::Right,
        };
        assert_eq!(
            transition(&s, &Event::ReachedEdge),
            Some(State::Walk {
                direction: Direction::Left
            })
        );
    }

    #[test]
    fn walk_climbs_nearby_window() {
        let s = State::Walk {
            direction: Direction::Right,
        };
        let ev = Event::WindowNearby {
            id: 42,
            side: WallSide::Left,
            x: 0.0,
            y: 0.0,
        };
        assert_eq!(
            transition(&s, &ev),
            Some(State::ClimbWall {
                window_id: 42,
                side: WallSide::Left
            })
        );
    }

    #[test]
    fn climb_top_returns_to_idle() {
        let s = State::ClimbWall {
            window_id: 1,
            side: WallSide::Right,
        };
        assert_eq!(transition(&s, &Event::ReachedEdge), Some(State::Idle));
    }

    #[test]
    fn climb_falls_when_window_gone() {
        let s = State::ClimbWall {
            window_id: 7,
            side: WallSide::Left,
        };
        assert_eq!(
            transition(&s, &Event::WindowGone { id: 7 }),
            Some(State::Fall {
                velocity: Vec2::ZERO
            })
        );
        // Une autre fenêtre disparaît : aucun changement.
        assert_eq!(transition(&s, &Event::WindowGone { id: 99 }), None);
    }

    #[test]
    fn fall_then_land_then_idle() {
        let falling = State::Fall {
            velocity: Vec2::new(0.0, 500.0),
        };
        assert_eq!(transition(&falling, &Event::ReachedEdge), Some(State::Land));
        assert_eq!(
            transition(&State::Land, &Event::LandingAnimDone),
            Some(State::Idle)
        );
    }

    #[test]
    fn drag_overrides_then_releases_to_fall() {
        assert_eq!(transition(&State::Idle, &Event::DragStart), Some(State::Dragged));
        let walk = State::Walk {
            direction: Direction::Left,
        };
        assert_eq!(transition(&walk, &Event::DragStart), Some(State::Dragged));
        // DragStart depuis Dragged ne fait rien.
        assert_eq!(transition(&State::Dragged, &Event::DragStart), None);

        let v = Vec2::new(10.0, -20.0);
        assert_eq!(
            transition(&State::Dragged, &Event::DragEnd { velocity: v }),
            Some(State::Fall { velocity: v })
        );
    }

    #[test]
    fn unhandled_events_return_none() {
        assert_eq!(transition(&State::Idle, &Event::ReachedEdge), None);
        assert_eq!(
            transition(&State::Idle, &Event::Tick { dt_ms: 16 }),
            None
        );
        assert_eq!(transition(&State::Land, &Event::ReachedEdge), None);
    }

    #[test]
    fn transition_is_pure() {
        // Appeler plusieurs fois ne change rien à l'entrée et donne le même résultat.
        let s = State::Idle;
        let ev = Event::IdleTimerFired;
        let a = transition(&s, &ev);
        let b = transition(&s, &ev);
        assert_eq!(a, b);
        assert_eq!(s, State::Idle);
    }
}