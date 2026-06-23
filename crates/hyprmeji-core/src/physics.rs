// crates/hyprmeji-core/src/physics.rs
//! Moteur physique 2D minimaliste.
//!
//! Met à jour position et vélocité à chaque `Tick`. Gère gravité, vitesse
//! terminale, collision avec le sol, les bords d'écran et les bords de fenêtre.
//! Aucun I/O : l'environnement (taille d'écran, fenêtres) est passé en argument.

use crate::state::{Direction, State, WallSide};
use crate::types::{Rect, Vec2};

/// Constantes physiques ajustables (alimentées depuis la config `[physics]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsConfig {
    /// Accélération de gravité, en px/s².
    pub gravity: f32,
    /// Vitesse de chute maximale (terminale), en px/s.
    pub max_fall_speed: f32,
    /// Vitesse de marche horizontale, en px/s.
    pub walk_speed: f32,
    /// Vitesse d'escalade verticale, en px/s.
    pub climb_speed: f32,
    /// Distance d'accrochage à un bord de fenêtre, en px.
    pub snap_distance: f32,
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Self {
            gravity: 1800.0,
            max_fall_speed: 1200.0,
            walk_speed: 80.0,
            climb_speed: 60.0,
            snap_distance: 4.0,
        }
    }
}

/// Corps physique du shimeji.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsBody {
    /// Position en pixels (coin haut-gauche du sprite).
    pub pos: Vec2,
    /// Vélocité en px/s.
    pub vel: Vec2,
    pub on_ground: bool,
    pub on_wall: bool,
}

impl PhysicsBody {
    /// Crée un corps au repos à la position donnée.
    #[must_use]
    pub fn new(pos: Vec2) -> Self {
        Self {
            pos,
            vel: Vec2::ZERO,
            on_ground: false,
            on_wall: false,
        }
    }
}

/// Dimensions de l'écran (moniteur principal en v1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub width: f32,
    pub height: f32,
}

/// Résultat d'un pas de simulation : signale si un bord a été atteint, afin que
/// l'appelant puisse émettre l'`Event::ReachedEdge` correspondant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepOutcome {
    /// Le sol a été touché pendant ce pas.
    pub hit_ground: bool,
    /// Un bord latéral d'écran a été atteint.
    pub hit_screen_edge: bool,
    /// Le sommet de la fenêtre escaladée a été atteint.
    pub reached_wall_top: bool,
}

impl StepOutcome {
    /// Indique si un quelconque bord a été atteint.
    #[must_use]
    pub fn reached_any_edge(self) -> bool {
        self.hit_ground || self.hit_screen_edge || self.reached_wall_top
    }
}

/// Moteur physique sans état (les constantes sont dans `config`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysicsEngine {
    pub config: PhysicsConfig,
}

impl PhysicsEngine {
    /// Construit un moteur avec une configuration donnée.
    #[must_use]
    pub fn new(config: PhysicsConfig) -> Self {
        Self { config }
    }

    /// Avance la simulation d'un pas.
    ///
    /// * `body`   — corps à mettre à jour (muté en place).
    /// * `state`  — état courant (détermine le comportement physique).
    /// * `dt_ms`  — temps écoulé en millisecondes.
    /// * `sprite` — dimensions du sprite courant (largeur, hauteur), en px.
    /// * `screen` — dimensions de l'écran.
    ///
    /// Retourne un [`StepOutcome`] décrivant les bords éventuellement atteints.
    pub fn step(
        &self,
        body: &mut PhysicsBody,
        state: &State,
        dt_ms: u32,
        sprite: (f32, f32),
        screen: Screen,
    ) -> StepOutcome {
        let dt = dt_ms as f32 / 1000.0;
        let (sprite_w, sprite_h) = sprite;
        let mut outcome = StepOutcome::default();

        match state {
            // Physique suspendue : le drag pilote directement la position.
            State::Dragged => {
                body.vel = Vec2::ZERO;
                body.on_ground = false;
                body.on_wall = false;
            }

            // Marche horizontale au sol.
            State::Walk { direction } => {
                let dir = match direction {
                    Direction::Left => -1.0,
                    Direction::Right => 1.0,
                };
                body.vel.x = dir * self.config.walk_speed;
                body.vel.y = 0.0;
                body.pos.x += body.vel.x * dt;
                body.on_ground = true;
                body.on_wall = false;
                outcome.hit_screen_edge = self.clamp_horizontal(body, sprite_w, screen);
            }

            // Escalade verticale : x verrouillé, y décroît.
            State::ClimbWall { side, .. } => {
                body.on_wall = true;
                body.on_ground = false;
                body.vel.x = 0.0;
                body.vel.y = -self.config.climb_speed;
                body.pos.y += body.vel.y * dt;
                // x est verrouillé au bord ; on conserve le côté pour info.
                let _ = side;
                if body.pos.y <= 0.0 {
                    body.pos.y = 0.0;
                    outcome.reached_wall_top = true;
                }
            }

            // Chute libre soumise à la gravité.
            State::Fall { .. } => {
                body.on_ground = false;
                body.on_wall = false;
                body.vel.y += self.config.gravity * dt;
                if body.vel.y > self.config.max_fall_speed {
                    body.vel.y = self.config.max_fall_speed;
                }
                body.pos += body.vel * dt;
                outcome.hit_screen_edge = self.clamp_horizontal(body, sprite_w, screen);
                outcome.hit_ground = self.clamp_ground(body, sprite_h, screen);
            }

            // Idle / Land : immobiles, mais soumis au sol par sécurité.
            State::Idle | State::Land => {
                body.vel = Vec2::ZERO;
                outcome.hit_ground = self.clamp_ground(body, sprite_h, screen);
                if outcome.hit_ground {
                    body.on_ground = true;
                }
            }
        }

        outcome
    }

    /// Détecte une fenêtre accrochable (`±snap_distance`) sous l'état `Walk`.
    ///
    /// Retourne le côté et l'identifiant de la première fenêtre adjacente, le
    /// cas échéant. Fonction de lecture seule (ne mute rien).
    #[must_use]
    pub fn detect_wall(
        &self,
        body: &PhysicsBody,
        sprite_w: f32,
        windows: &[(u32, Rect)],
    ) -> Option<(u32, WallSide)> {
        let d = self.config.snap_distance;
        let right_edge = body.pos.x + sprite_w;
        for (id, win) in windows {
            // Bord gauche de la fenêtre touché par la droite du shimeji.
            if (right_edge - win.x).abs() <= d {
                return Some((*id, WallSide::Left));
            }
            // Bord droit de la fenêtre touché par la gauche du shimeji.
            if (body.pos.x - (win.x + win.w)).abs() <= d {
                return Some((*id, WallSide::Right));
            }
        }
        None
    }

    /// Contraint la position horizontale dans l'écran. Retourne `true` si un
    /// bord latéral a été atteint.
    fn clamp_horizontal(&self, body: &mut PhysicsBody, sprite_w: f32, screen: Screen) -> bool {
        if body.pos.x <= 0.0 {
            body.pos.x = 0.0;
            true
        } else if body.pos.x + sprite_w >= screen.width {
            body.pos.x = screen.width - sprite_w;
            true
        } else {
            false
        }
    }

    /// Contraint la position verticale au sol. Retourne `true` si le sol a été
    /// atteint pendant ce pas.
    fn clamp_ground(&self, body: &mut PhysicsBody, sprite_h: f32, screen: Screen) -> bool {
        if body.pos.y + sprite_h >= screen.height {
            body.pos.y = screen.height - sprite_h;
            body.vel.y = 0.0;
            body.on_ground = true;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Direction;

    const SCREEN: Screen = Screen {
        width: 1920.0,
        height: 1080.0,
    };
    const SPRITE: (f32, f32) = (128.0, 128.0);

    fn engine() -> PhysicsEngine {
        PhysicsEngine::default()
    }

    #[test]
    fn default_config_matches_architecture() {
        let c = PhysicsConfig::default();
        assert_eq!(c.gravity, 1800.0);
        assert_eq!(c.max_fall_speed, 1200.0);
        assert_eq!(c.walk_speed, 80.0);
        assert_eq!(c.climb_speed, 60.0);
    }

    #[test]
    fn fall_applies_gravity() {
        let e = engine();
        let mut body = PhysicsBody::new(Vec2::new(100.0, 100.0));
        let st = State::Fall {
            velocity: Vec2::ZERO,
        };
        let out = e.step(&mut body, &st, 16, SPRITE, SCREEN);
        assert!(body.vel.y > 0.0);
        assert!(body.pos.y > 100.0);
        assert!(!out.hit_ground);
    }

    #[test]
    fn fall_respects_terminal_velocity() {
        let e = engine();
        let mut body = PhysicsBody::new(Vec2::new(100.0, 0.0));
        body.vel.y = 5000.0;
        let st = State::Fall {
            velocity: Vec2::ZERO,
        };
        e.step(&mut body, &st, 16, SPRITE, SCREEN);
        assert_eq!(body.vel.y, e.config.max_fall_speed);
    }

    #[test]
    fn fall_hits_ground() {
        let e = engine();
        let mut body = PhysicsBody::new(Vec2::new(100.0, 1070.0));
        body.vel.y = 800.0;
        let st = State::Fall {
            velocity: Vec2::ZERO,
        };
        let out = e.step(&mut body, &st, 16, SPRITE, SCREEN);
        assert!(out.hit_ground);
        assert!(body.on_ground);
        assert_eq!(body.vel.y, 0.0);
        assert_eq!(body.pos.y, SCREEN.height - SPRITE.1);
    }

    #[test]
    fn walk_moves_right_and_left() {
        let e = engine();
        let mut body = PhysicsBody::new(Vec2::new(500.0, 952.0));
        let st_r = State::Walk {
            direction: Direction::Right,
        };
        e.step(&mut body, &st_r, 1000, SPRITE, SCREEN);
        assert!((body.pos.x - (500.0 + e.config.walk_speed)).abs() < 0.01);

        let mut body2 = PhysicsBody::new(Vec2::new(500.0, 952.0));
        let st_l = State::Walk {
            direction: Direction::Left,
        };
        e.step(&mut body2, &st_l, 1000, SPRITE, SCREEN);
        assert!((body2.pos.x - (500.0 - e.config.walk_speed)).abs() < 0.01);
    }

    #[test]
    fn walk_hits_screen_edge() {
        let e = engine();
        let mut body = PhysicsBody::new(Vec2::new(2.0, 952.0));
        let st = State::Walk {
            direction: Direction::Left,
        };
        let out = e.step(&mut body, &st, 1000, SPRITE, SCREEN);
        assert!(out.hit_screen_edge);
        assert_eq!(body.pos.x, 0.0);
    }

    #[test]
    fn climb_moves_up_and_reaches_top() {
        let e = engine();
        let mut body = PhysicsBody::new(Vec2::new(300.0, 1.0));
        let st = State::ClimbWall {
            window_id: 1,
            side: WallSide::Left,
        };
        let out = e.step(&mut body, &st, 1000, SPRITE, SCREEN);
        assert!(out.reached_wall_top);
        assert_eq!(body.pos.y, 0.0);
        assert!(body.on_wall);
    }

    #[test]
    fn dragged_suspends_physics() {
        let e = engine();
        let mut body = PhysicsBody::new(Vec2::new(100.0, 100.0));
        body.vel = Vec2::new(50.0, 50.0);
        let out = e.step(&mut body, &State::Dragged, 16, SPRITE, SCREEN);
        assert_eq!(body.vel, Vec2::ZERO);
        assert_eq!(body.pos, Vec2::new(100.0, 100.0));
        assert!(!out.reached_any_edge());
    }

    #[test]
    fn detect_wall_finds_adjacent_window() {
        let e = engine();
        let body = PhysicsBody::new(Vec2::new(100.0, 500.0));
        // Bord gauche de la fenêtre à x=230, droite du shimeji à 100+128=228 (≤4px).
        let windows = vec![(5u32, Rect::new(230.0, 400.0, 300.0, 300.0))];
        assert_eq!(
            e.detect_wall(&body, 128.0, &windows),
            Some((5, WallSide::Left))
        );
    }

    #[test]
    fn detect_wall_none_when_far() {
        let e = engine();
        let body = PhysicsBody::new(Vec2::new(100.0, 500.0));
        let windows = vec![(5u32, Rect::new(900.0, 400.0, 300.0, 300.0))];
        assert_eq!(e.detect_wall(&body, 128.0, &windows), None);
    }
}