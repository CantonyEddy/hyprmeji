// crates/hyprmeji-input/src/lib.rs
#![deny(clippy::all)]
//! # hyprmeji-input
//!
//! Gestion des interactions souris (`wl_pointer`) sur la surface layer-shell
//! créée par `hyprmeji-render`. Détecte le clic + drag sur le shimeji et émet
//! des [`InputEvent`] consommés par la boucle principale.
//!
//! **Invariants du crate :** ne crée aucune surface Wayland (c'est le rôle de
//! `hyprmeji-render`) ; il s'abonne uniquement au `wl_pointer` d'un `WlSeat`
//! fourni. Aucun `main`.
//!
//! ## Architecture interne
//! - [`handler::DragTracker`] : machine d'états *pure* du drag (idle ↔ dragging),
//!   calcul de la vélocité moyenne et du `grab_offset`. Testable sans Wayland.
//! - [`handler::InputHandler`] : adaptateur `wl_pointer` qui alimente le tracker
//!   à partir des événements Wayland et expose [`InputHandler::poll`].
//!
//! ## Exemple (non compilable hors compositor)
//! ```no_run
//! # use hyprmeji_input::InputHandler;
//! # fn demo(handler: &mut InputHandler) {
//! while let Some(event) = handler.poll() {
//!     // dispatch vers la state machine de hyprmeji-core…
//!     let _ = event;
//! }
//! # }
//! ```

mod error;
mod handler;

pub use error::InputError;
pub use handler::{InputEvent, InputHandler};