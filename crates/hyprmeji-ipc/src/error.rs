// crates/hyprmeji-ipc/src/error.rs
//! Types d'erreur publics du crate, basés sur `thiserror`.

use std::path::PathBuf;

use thiserror::Error;

/// Erreurs renvoyées par les API publiques de `hyprmeji-ipc`.
#[derive(Debug, Error)]
pub enum IpcError {
    /// La variable d'environnement `HYPRLAND_INSTANCE_SIGNATURE` est absente.
    ///
    /// Sans elle, impossible de localiser les sockets Hyprland.
    #[error("variable d'environnement HYPRLAND_INSTANCE_SIGNATURE introuvable (Hyprland est-il lancé ?)")]
    MissingSignature(#[source] std::env::VarError),

    /// Échec de connexion à un socket Hyprland.
    #[error("connexion impossible au socket {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Erreur d'I/O lors d'un échange sur le socket.
    #[error("erreur d'I/O sur {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// La réponse JSON de `j/clients` n'a pas pu être désérialisée.
    #[error("réponse j/clients invalide : {0}")]
    JsonParse(#[from] serde_json::Error),
}

impl IpcError {
    /// Helper : construit une [`IpcError::Connect`] en attachant le chemin.
    pub(crate) fn connect(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        IpcError::Connect {
            path: path.into(),
            source,
        }
    }

    /// Helper : construit une [`IpcError::Io`] en attachant le chemin.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        IpcError::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn missing_signature_message_is_readable() {
        let e = IpcError::MissingSignature(std::env::VarError::NotPresent);
        let msg = e.to_string();
        assert!(msg.contains("HYPRLAND_INSTANCE_SIGNATURE"));
    }

    #[test]
    fn connect_message_contains_path() {
        let src = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let e = IpcError::connect(Path::new("/tmp/hypr/abc/.socket.sock"), src);
        assert!(e.to_string().contains(".socket.sock"));
    }

    #[test]
    fn io_message_contains_path() {
        let src = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken");
        let e = IpcError::io(Path::new("/tmp/hypr/abc/.socket2.sock"), src);
        assert!(e.to_string().contains(".socket2.sock"));
    }

    #[test]
    fn json_error_is_convertible() {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str("{ invalid");
        let e: IpcError = parsed.unwrap_err().into();
        assert!(matches!(e, IpcError::JsonParse(_)));
        assert!(e.to_string().contains("j/clients"));
    }
}