// crates/hyprmeji-render/src/lib.rs
#![deny(clippy::all)]
//! # hyprmeji-render
//!
//! Rendu d'un shimeji sur une surface Wayland overlay via `wlr-layer-shell`.
//!
//! Ce crate crée et gère une surface `zwlr_layer_surface_v1` de type **overlay**,
//! transparente, avec input passthrough par défaut. Il alloue des buffers
//! `wl_shm` en double buffering, rasterise les frames RGBA pré-décodées
//! (fournies par `hyprmeji-core::AnimationFrame`) après conversion en ARGB8888,
//! applique le miroir horizontal (`flip_x`) en mémoire, et met à jour l'input
//! region en pixel-perfect (seuls les pixels d'alpha `> 0` sont saisissables).
//!
//! **Invariants du crate :**
//! - aucun décodage PNG ici (les frames arrivent déjà décodées) ;
//! - aucune logique métier (machine d'états, physique → `hyprmeji-core`) ;
//! - aucune gestion souris (→ `hyprmeji-input`) ;
//! - aucun `main`.
//!
//! ## API publique
//! Tout passe par [`Renderer`] :
//! ```no_run
//! # use hyprmeji_render::Renderer;
//! # use hyprmeji_core::{AnimationFrame, Vec2};
//! # fn demo(frame: &AnimationFrame) -> Result<(), hyprmeji_render::RenderError> {
//! let mut renderer = Renderer::new()?;
//! renderer.render_frame(frame, Vec2::new(100.0, 200.0))?;
//! renderer.set_input_region(frame, Vec2::new(100.0, 200.0))?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Organisation interne
//! - [`error`] — [`RenderError`] (`thiserror`).
//! - `surface` — création/gestion de la layer surface et logique d'input region.
//! - `buffer` — pool `wl_shm` double buffering + transformations pixel pures.
//! - `renderer` — [`Renderer`], état SCTK et boucle de rendu.
//!
//! La logique pure (swizzle RGBA→ARGB8888, flip horizontal, calcul des marges,
//! découpage de l'input region) est isolée et testée sans compositor ; les tests
//! d'intégration nécessitant un vrai compositor Wayland sont hors scope.

mod buffer;
mod error;
mod renderer;
mod surface;

pub use error::RenderError;
pub use renderer::Renderer;

// Ré-exports de confort : les types d'entrée de l'API proviennent du core.
pub use hyprmeji_core::{AnimationFrame, Vec2};