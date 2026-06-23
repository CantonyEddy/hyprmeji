// crates/hyprmeji-render/src/error.rs
//! Types d'erreur publics du crate, basés sur `thiserror`.

use thiserror::Error;

/// Erreurs renvoyées par les API publiques de `hyprmeji-render`.
#[derive(Debug, Error)]
pub enum RenderError {
    /// Impossible d'établir la connexion au compositor Wayland.
    #[error("connexion Wayland impossible : {0}")]
    Connect(String),

    /// Un global Wayland requis est absent du compositor.
    ///
    /// Typiquement `zwlr_layer_shell_v1` (compositor non wlroots) ou `wl_shm`.
    #[error("global Wayland requis absent : {0}")]
    MissingGlobal(&'static str),

    /// Aucun moniteur (`wl_output`) disponible pour ancrer la surface.
    #[error("aucun moniteur disponible")]
    NoOutput,

    /// Échec d'un round-trip / dispatch de la file d'événements Wayland.
    #[error("échec du dispatch Wayland : {0}")]
    Dispatch(String),

    /// Échec d'allocation d'un buffer `wl_shm` (mémoire partagée).
    #[error("allocation du buffer shm échouée : {0}")]
    BufferAlloc(String),

    /// La surface n'a pas encore reçu sa première configuration du compositor.
    ///
    /// `render_frame` ne peut commit qu'après le premier `configure`.
    #[error("la layer surface n'est pas encore configurée")]
    NotConfigured,

    /// Dimensions de frame invalides (largeur ou hauteur nulle).
    #[error("dimensions de frame invalides : {width}x{height}")]
    InvalidDimensions {
        /// Largeur fautive, en pixels.
        width: u32,
        /// Hauteur fautive, en pixels.
        height: u32,
    },

    /// Le buffer de pixels fourni n'a pas la taille attendue.
    #[error("taille de buffer incohérente : {got} octets, attendu {expected}")]
    BufferSizeMismatch {
        /// Taille effective du buffer, en octets.
        got: usize,
        /// Taille attendue (`width * height * 4`), en octets.
        expected: usize,
    },
}