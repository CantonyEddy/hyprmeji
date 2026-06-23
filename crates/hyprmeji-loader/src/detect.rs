// crates/hyprmeji-loader/src/detect.rs
//! Détection du format d'un répertoire shimeji.
//!
//! La détection est purement basée sur la présence de fichiers marqueurs :
//! - `manifest.toml` → [`ShimejiFormat::Native`] ;
//! - `actions.xml`   → [`ShimejiFormat::Java`].
//!
//! Le format natif est prioritaire si les deux marqueurs coexistent.

use std::path::Path;

use crate::error::LoaderError;

/// Nom du fichier marqueur du format natif.
pub(crate) const NATIVE_MANIFEST: &str = "manifest.toml";
/// Nom du fichier marqueur du format Java shimeji.
pub(crate) const JAVA_ACTIONS: &str = "actions.xml";

/// Format d'un répertoire shimeji.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimejiFormat {
    /// Format natif `manifest.toml`.
    Native,
    /// Format Java shimeji (`actions.xml` + `img/`).
    Java,
}

/// Détecte le format de `dir`.
///
/// # Erreurs
/// - [`LoaderError::NotADirectory`] si `dir` n'est pas un répertoire ;
/// - [`LoaderError::UnknownFormat`] si aucun marqueur n'est présent.
pub(crate) fn detect_format(dir: &Path) -> Result<ShimejiFormat, LoaderError> {
    if !dir.is_dir() {
        return Err(LoaderError::NotADirectory(dir.to_path_buf()));
    }
    // Le natif est prioritaire si les deux coexistent.
    if dir.join(NATIVE_MANIFEST).is_file() {
        Ok(ShimejiFormat::Native)
    } else if dir.join(JAVA_ACTIONS).is_file() {
        Ok(ShimejiFormat::Java)
    } else {
        Err(LoaderError::UnknownFormat(dir.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Crée un répertoire temporaire isolé. On nettoie en fin de test.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let unique = format!(
                "hyprmeji-loader-test-{}-{}",
                tag,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            p.push(unique);
            fs::create_dir_all(&p).expect("create temp dir");
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn touch(&self, name: &str) {
            fs::write(self.0.join(name), b"x").expect("touch file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn detects_native() {
        let d = TempDir::new("native");
        d.touch(NATIVE_MANIFEST);
        assert_eq!(detect_format(d.path()).ok(), Some(ShimejiFormat::Native));
    }

    #[test]
    fn detects_java() {
        let d = TempDir::new("java");
        d.touch(JAVA_ACTIONS);
        assert_eq!(detect_format(d.path()).ok(), Some(ShimejiFormat::Java));
    }

    #[test]
    fn native_wins_over_java() {
        let d = TempDir::new("both");
        d.touch(NATIVE_MANIFEST);
        d.touch(JAVA_ACTIONS);
        assert_eq!(detect_format(d.path()).ok(), Some(ShimejiFormat::Native));
    }

    #[test]
    fn unknown_when_empty() {
        let d = TempDir::new("empty");
        assert!(matches!(
            detect_format(d.path()),
            Err(LoaderError::UnknownFormat(_))
        ));
    }

    #[test]
    fn not_a_directory() {
        let missing = std::env::temp_dir().join("hyprmeji-does-not-exist-zzz");
        assert!(matches!(
            detect_format(&missing),
            Err(LoaderError::NotADirectory(_))
        ));
    }
}