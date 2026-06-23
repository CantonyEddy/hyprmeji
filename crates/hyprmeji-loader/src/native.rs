// crates/hyprmeji-loader/src/native.rs
//! Parsing du format natif `manifest.toml`.
//!
//! Le schéma suivi est celui de `ARCHITECTURE.md` §5. Chaque frame référence un
//! fichier PNG (`file`) et une durée (`duration_ms`). Les chemins sont résolus
//! relativement au répertoire du shimeji.

use std::collections::HashMap;
use std::path::Path;

use hyprmeji_core::{AnimationFrame, SpriteSheet};
use serde::Deserialize;

use crate::detect::NATIVE_MANIFEST;
use crate::error::LoaderError;

/// Racine désérialisée de `manifest.toml`.
///
/// Seul le bloc `[[animations]]` est nécessaire pour produire la `SpriteSheet`.
/// Les autres blocs (`[shimeji]`, `[sprites]`, `[physics]`) sont acceptés mais
/// ignorés ici — ils sont consommés ailleurs dans le pipeline.
#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    animations: Vec<AnimationDef>,
}

/// Définition d'une animation dans le manifeste.
#[derive(Debug, Deserialize)]
struct AnimationDef {
    name: String,
    #[serde(default)]
    frames: Vec<FrameDef>,
    /// Miroir horizontal global pour toutes les frames de l'animation.
    #[serde(default)]
    flip_x: bool,
}

/// Définition d'une frame : fichier PNG + durée d'affichage.
#[derive(Debug, Deserialize)]
struct FrameDef {
    file: String,
    duration_ms: u32,
}

/// Charge un shimeji au format natif depuis `dir`.
pub(crate) fn load_native(dir: &Path) -> Result<SpriteSheet, LoaderError> {
    let manifest_path = dir.join(NATIVE_MANIFEST);
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| LoaderError::io(&manifest_path, e))?;
    let manifest: Manifest = toml::from_str(&raw)?;
    build_sheet(dir, &manifest)
}

/// Construit la `SpriteSheet` à partir d'un manifeste déjà parsé.
///
/// Séparé de [`load_native`] pour permettre des tests sans I/O sur le parsing,
/// et avec un répertoire de fixtures pour le décodage PNG.
fn build_sheet(dir: &Path, manifest: &Manifest) -> Result<SpriteSheet, LoaderError> {
    let mut animations: HashMap<String, Vec<AnimationFrame>> = HashMap::new();

    for anim in &manifest.animations {
        if anim.frames.is_empty() {
            log::warn!("animation `{}` sans frame, ignorée", anim.name);
            continue;
        }
        let mut frames = Vec::with_capacity(anim.frames.len());
        for fr in &anim.frames {
            let frame = decode_frame(dir, &fr.file, fr.duration_ms, anim.flip_x)?;
            frames.push(frame);
        }
        animations.insert(anim.name.clone(), frames);
    }

    if animations.is_empty() {
        return Err(LoaderError::NoAnimations(dir.to_path_buf()));
    }

    Ok(SpriteSheet { animations })
}

/// Décode un PNG en [`AnimationFrame`] RGBA. `rel` est relatif à `dir`.
pub(crate) fn decode_frame(
    dir: &Path,
    rel: &str,
    duration_ms: u32,
    flip_x: bool,
) -> Result<AnimationFrame, LoaderError> {
    let path = dir.join(rel);
    let img = image::open(&path).map_err(|e| LoaderError::image(&path, e))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = rgba.into_raw();
    Ok(AnimationFrame {
        pixels: pixels.into(),
        width,
        height,
        duration_ms,
        flip_x,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Répertoire temporaire jetable.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "hyprmeji-native-{}-{}",
                tag,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(p.join("img")).expect("mkdir");
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        /// Écrit un PNG `w`x`h` rouge opaque sous `img/<name>`.
        fn write_png(&self, name: &str, w: u32, h: u32) {
            let buf = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
            buf.save(self.0.join("img").join(name)).expect("save png");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_manifest_and_decodes() {
        let d = TempDir::new("ok");
        d.write_png("idle_0.png", 4, 6);
        d.write_png("idle_1.png", 4, 6);

        let manifest: Manifest = toml::from_str(
            r#"
            [[animations]]
            name = "idle"
            frames = [
              { file = "img/idle_0.png", duration_ms = 500 },
              { file = "img/idle_1.png", duration_ms = 300 },
            ]
            "#,
        )
        .expect("toml ok");

        let sheet = build_sheet(d.path(), &manifest).expect("build ok");
        let idle = sheet.animation("idle").expect("idle present");
        assert_eq!(idle.len(), 2);
        assert_eq!(idle[0].width, 4);
        assert_eq!(idle[0].height, 6);
        assert_eq!(idle[0].duration_ms, 500);
        assert_eq!(idle[1].duration_ms, 300);
        // RGBA : 4*6*4 octets.
        assert_eq!(idle[0].pixels.len(), 4 * 6 * 4);
    }

    #[test]
    fn flip_x_propagated_to_frames() {
        let d = TempDir::new("flip");
        d.write_png("w.png", 2, 2);
        let manifest: Manifest = toml::from_str(
            r#"
            [[animations]]
            name = "walk"
            flip_x = true
            frames = [{ file = "img/w.png", duration_ms = 100 }]
            "#,
        )
        .expect("toml ok");
        let sheet = build_sheet(d.path(), &manifest).expect("build ok");
        assert!(sheet.animation("walk").expect("walk")[0].flip_x);
    }

    #[test]
    fn empty_animation_is_skipped() {
        let d = TempDir::new("skip");
        d.write_png("i.png", 1, 1);
        let manifest: Manifest = toml::from_str(
            r#"
            [[animations]]
            name = "empty"
            frames = []

            [[animations]]
            name = "idle"
            frames = [{ file = "img/i.png", duration_ms = 100 }]
            "#,
        )
        .expect("toml ok");
        let sheet = build_sheet(d.path(), &manifest).expect("build ok");
        assert!(sheet.animation("empty").is_none());
        assert!(sheet.animation("idle").is_some());
    }

    #[test]
    fn no_animations_is_error() {
        let d = TempDir::new("none");
        let manifest: Manifest = toml::from_str("").expect("toml ok");
        assert!(matches!(
            build_sheet(d.path(), &manifest),
            Err(LoaderError::NoAnimations(_))
        ));
    }

    #[test]
    fn missing_png_is_error() {
        let d = TempDir::new("missing");
        let manifest: Manifest = toml::from_str(
            r#"
            [[animations]]
            name = "idle"
            frames = [{ file = "img/nope.png", duration_ms = 100 }]
            "#,
        )
        .expect("toml ok");
        assert!(matches!(
            build_sheet(d.path(), &manifest),
            Err(LoaderError::ImageDecode { .. })
        ));
    }

    #[test]
    fn paths_are_resolved_relative_to_dir() {
        // Le fichier est référencé sans préfixe absolu et résolu sous `dir`.
        let d = TempDir::new("rel");
        d.write_png("r.png", 3, 3);
        let frame = decode_frame(d.path(), "img/r.png", 50, false).expect("decode");
        assert_eq!((frame.width, frame.height), (3, 3));
    }
}