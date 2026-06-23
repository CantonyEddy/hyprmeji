// crates/hyprmeji-core/src/error.rs
//! Types d'erreur publics du crate, basés sur `thiserror`.

use thiserror::Error;

/// Erreurs renvoyées par les API publiques de `hyprmeji-core`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    /// Animation référencée mais absente de la `SpriteSheet`.
    #[error("animation introuvable : `{name}`")]
    MissingAnimation { name: String },

    /// Animation présente mais ne contenant aucune frame.
    #[error("animation vide : `{name}`")]
    EmptyAnimation { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages() {
        let e = CoreError::MissingAnimation {
            name: "walk".into(),
        };
        assert!(e.to_string().contains("walk"));
        let e2 = CoreError::EmptyAnimation { name: "idle".into() };
        assert!(e2.to_string().contains("idle"));
    }
}