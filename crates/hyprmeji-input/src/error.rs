// crates/hyprmeji-input/src/error.rs
//! Types d'erreur publics du crate, basés sur `thiserror`.

use thiserror::Error;

/// Erreurs renvoyées par les API publiques de `hyprmeji-input`.
#[derive(Debug, Error)]
pub enum InputError {
    /// Le seat fourni n'expose pas de capacité pointeur (`wl_pointer`).
    ///
    /// Sans pointeur, aucun drag ne peut être détecté.
    #[error("le seat ne fournit pas de capacité pointeur (wl_pointer)")]
    NoPointer,

    /// Échec d'initialisation du pointeur côté smithay-client-toolkit.
    #[error("initialisation du pointeur échouée : {0}")]
    PointerInit(String),
}