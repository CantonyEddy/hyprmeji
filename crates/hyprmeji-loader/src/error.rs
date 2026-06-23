// crates/hyprmeji-loader/src/error.rs
//! Types d'erreur publics du crate, basés sur `thiserror`.

use std::path::PathBuf;

use thiserror::Error;

/// Erreurs renvoyées par les API publiques de `hyprmeji-loader`.
#[derive(Debug, Error)]
pub enum LoaderError {
    /// Le chemin fourni n'existe pas ou n'est pas un répertoire.
    #[error("le chemin n'est pas un répertoire shimeji valide : {0}")]
    NotADirectory(PathBuf),

    /// Aucun format reconnu (`manifest.toml` ou `actions.xml`) dans le dossier.
    #[error("format de shimeji non reconnu dans : {0} (ni manifest.toml ni actions.xml)")]
    UnknownFormat(PathBuf),

    /// Erreur d'I/O lors de la lecture d'un fichier.
    #[error("erreur d'I/O sur {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Échec du parsing TOML du manifeste natif.
    #[error("manifest.toml invalide : {0}")]
    TomlParse(#[from] toml::de::Error),

    /// Échec du parsing XML des actions Java.
    #[error("actions.xml invalide : {0}")]
    XmlParse(String),

    /// Échec du décodage d'un PNG.
    #[error("décodage PNG impossible pour {path}: {source}")]
    ImageDecode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },

    /// Le manifeste / les actions ne contiennent aucune animation exploitable.
    #[error("aucune animation exploitable trouvée dans : {0}")]
    NoAnimations(PathBuf),
}

impl LoaderError {
    /// Helper : construit une [`LoaderError::Io`] en attachant le chemin fautif.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        LoaderError::Io {
            path: path.into(),
            source,
        }
    }

    /// Helper : construit une [`LoaderError::ImageDecode`].
    pub(crate) fn image(path: impl Into<PathBuf>, source: image::ImageError) -> Self {
        LoaderError::ImageDecode {
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
    fn messages_contain_path() {
        let e = LoaderError::NotADirectory(PathBuf::from("/foo/bar"));
        assert!(e.to_string().contains("/foo/bar"));

        let e = LoaderError::UnknownFormat(PathBuf::from("/baz"));
        assert!(e.to_string().contains("/baz"));
    }

    #[test]
    fn io_helper_attaches_path() {
        let src = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let e = LoaderError::io(Path::new("x.png"), src);
        assert!(e.to_string().contains("x.png"));
    }

    #[test]
    fn toml_error_is_convertible() {
        let bad = "this = = invalid";
        let parsed: Result<toml::Value, _> = toml::from_str(bad);
        let e: LoaderError = parsed.unwrap_err().into();
        assert!(matches!(e, LoaderError::TomlParse(_)));
    }
}