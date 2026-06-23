// crates/hyprmeji-loader/src/lib.rs
#![deny(clippy::all)]
//! # hyprmeji-loader
//!
//! Chargement de shimejis depuis un répertoire, vers une [`SpriteSheet`]
//! prête à l'emploi pour `hyprmeji-core`.
//!
//! Deux formats sont supportés :
//! - **natif** : un `manifest.toml` (voir `ARCHITECTURE.md` §5) ;
//! - **Java shimeji** (import) : un `actions.xml` + un dossier `img/`.
//!
//! Le format est détecté automatiquement à partir du contenu du répertoire.
//!
//! **Invariants du crate :** aucune dépendance Wayland. Les seuls I/O sont la
//! lecture du répertoire shimeji et le décodage des PNG référencés. Tous les
//! chemins de fichiers sont résolus relativement au répertoire passé en
//! argument — jamais de chemin absolu hardcodé.
//!
//! ## Point d'entrée
//! ```no_run
//! use std::path::Path;
//! let sheet = hyprmeji_loader::load(Path::new("assets/default-shimeji"))?;
//! # Ok::<(), hyprmeji_loader::LoaderError>(())
//! ```

mod detect;
mod error;
mod java;
mod native;

use std::path::Path;

use hyprmeji_core::SpriteSheet;

pub use detect::ShimejiFormat;
pub use error::LoaderError;

/// Charge un shimeji depuis `path` et retourne sa [`SpriteSheet`].
///
/// Le format (natif `manifest.toml` ou Java `actions.xml`) est détecté
/// automatiquement via [`detect::detect_format`].
///
/// # Erreurs
/// Retourne une [`LoaderError`] si :
/// - le chemin n'est pas un répertoire lisible ;
/// - aucun format reconnu n'est trouvé ([`LoaderError::UnknownFormat`]) ;
/// - le parsing du manifeste / des actions échoue ;
/// - un PNG référencé est introuvable ou ne peut être décodé.
pub fn load(path: &Path) -> Result<SpriteSheet, LoaderError> {
    let format = detect::detect_format(path)?;
    log::info!("format détecté pour {:?} : {:?}", path, format);
    match format {
        ShimejiFormat::Native => native::load_native(path),
        ShimejiFormat::Java => java::load_java(path),
    }
}