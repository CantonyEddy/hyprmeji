// crates/hyprmeji-core/src/lib.rs
#![deny(clippy::all)]
//! # hyprmeji-core
//!
//! Logique métier pure du shimeji hyprmeji : machine d'états, physique 2D et
//! lecture d'animations.
//!
//! **Invariants du crate :** zéro I/O, zéro Wayland, zéro filesystem. Tout est
//! testable en isolation complète. Aucune dépendance système.
//!
//! ## Modules
//! - [`types`] — types partagés (`Vec2`, `Rect`, `AnimationFrame`, `SpriteSheet`).
//! - [`state`] — `State`, `Event` et la fonction de transition pure.
//! - [`physics`] — `PhysicsBody`, `PhysicsEngine` et ses règles de collision.
//! - [`animation`] — `AnimationPlayer`.

pub mod animation;
pub mod error;
pub mod physics;
pub mod state;
pub mod types;

pub use animation::AnimationPlayer;
pub use error::CoreError;
pub use physics::{PhysicsBody, PhysicsConfig, PhysicsEngine, Screen, StepOutcome};
pub use state::{transition, Direction, Event, State, WallSide};
pub use types::{AnimationFrame, Rect, SpriteSheet, Vec2};