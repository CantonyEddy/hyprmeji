// crates/hyprmeji-loader/src/java.rs
//! Import du format Java shimeji : `actions.xml` + dossier `img/`.
//!
//! On extrait des `actions.xml` les séquences d'animation (un `Action` de type
//! animation contient des `Frame` référençant une image et une durée), puis on
//! mappe le nom de l'action Java vers l'un des noms internes de `hyprmeji-core`
//! (`idle`, `walk`, `fall`, `climb`, `drag`, `land`). Les actions non mappables
//! sont ignorées avec un `log::warn!`.
//!
//! Le format `actions.xml` réel des shimejis Java est riche et variable ; on en
//! supporte un sous-ensemble pragmatique suffisant pour la v1. Les durées Java
//! sont exprimées en *ticks* (~40 ms/tick historiquement) et converties en ms.

use std::collections::HashMap;
use std::path::Path;

use hyprmeji_core::{AnimationFrame, SpriteSheet};
use serde::Deserialize;

use crate::detect::JAVA_ACTIONS;
use crate::error::LoaderError;
use crate::native::decode_frame;

/// Durée d'un tick Java en millisecondes (heuristique standard ~25 fps).
const TICK_MS: u32 = 40;
/// Durée par défaut si une frame n'indique pas de durée exploitable.
const DEFAULT_FRAME_MS: u32 = 100;

/// Racine `<Mascot>` / `<ActionList>` du fichier `actions.xml`.
///
/// On reste tolérant : `quick-xml` en mode serde mappe les éléments répétés
/// `<Action>` vers un `Vec`. Les attributs sont préfixés `@` par convention
/// quick-xml.
#[derive(Debug, Deserialize)]
struct ActionsRoot {
    #[serde(rename = "Action", default)]
    actions: Vec<Action>,
}

/// Une action Java. Seules les actions animées (avec des `<Animation>`/`<Pose>`)
/// nous intéressent.
#[derive(Debug, Deserialize)]
struct Action {
    #[serde(rename = "@Name", default)]
    name: String,
    #[serde(rename = "@Type", default)]
    type_: String,
    #[serde(rename = "Animation", default)]
    animations: Vec<Animation>,
}

/// Bloc `<Animation>` contenant une liste de poses.
#[derive(Debug, Deserialize)]
struct Animation {
    #[serde(rename = "Pose", default)]
    poses: Vec<Pose>,
}

/// Une pose : une image et une durée (en ticks).
#[derive(Debug, Deserialize)]
struct Pose {
    #[serde(rename = "@Image", default)]
    image: String,
    #[serde(rename = "@Duration", default)]
    duration: Option<u32>,
}

/// Charge un shimeji au format Java depuis `dir`.
pub(crate) fn load_java(dir: &Path) -> Result<SpriteSheet, LoaderError> {
    let actions_path = dir.join(JAVA_ACTIONS);
    let raw = std::fs::read_to_string(&actions_path)
        .map_err(|e| LoaderError::io(&actions_path, e))?;
    let root: ActionsRoot =
        quick_xml::de::from_str(&raw).map_err(|e| LoaderError::XmlParse(e.to_string()))?;
    build_sheet(dir, &root)
}

/// Mappe un nom d'action Java (insensible à la casse) vers un nom interne.
///
/// Retourne `None` si l'action n'est pas mappable (sera ignorée avec warning).
fn map_action_name(java_name: &str) -> Option<&'static str> {
    let n = java_name.to_ascii_lowercase();
    // Heuristiques de mapping : on cherche des sous-chaînes caractéristiques.
    if n.contains("stand") || n.contains("sit") || n == "idle" {
        Some("idle")
    } else if n.contains("walk") || n.contains("run") {
        Some("walk")
    } else if n.contains("fall") {
        Some("fall")
    } else if n.contains("climb") || n.contains("grab") || n.contains("wall") {
        Some("climb")
    } else if n.contains("drag") || n.contains("pinch") {
        Some("drag")
    } else if n.contains("land") || n.contains("bounce") {
        Some("land")
    } else {
        None
    }
}

/// Construit la `SpriteSheet` à partir d'un arbre `actions.xml` déjà parsé.
fn build_sheet(dir: &Path, root: &ActionsRoot) -> Result<SpriteSheet, LoaderError> {
    let mut animations: HashMap<String, Vec<AnimationFrame>> = HashMap::new();

    for action in &root.actions {
        let Some(internal) = map_action_name(&action.name) else {
            log::warn!(
                "action Java non mappable, ignorée : `{}` (type `{}`)",
                action.name,
                action.type_
            );
            continue;
        };

        // Aplatis toutes les poses de toutes les animations de l'action.
        let poses: Vec<&Pose> = action
            .animations
            .iter()
            .flat_map(|a| a.poses.iter())
            .collect();

        if poses.is_empty() {
            log::warn!("action `{}` sans pose, ignorée", action.name);
            continue;
        }

        // Si plusieurs actions mappent vers le même nom interne, on conserve la
        // première rencontrée et on ignore les suivantes (warning).
        if animations.contains_key(internal) {
            log::warn!(
                "action `{}` mappe vers `{}` déjà défini, ignorée",
                action.name,
                internal
            );
            continue;
        }

        let mut frames = Vec::with_capacity(poses.len());
        for pose in poses {
            let rel = normalize_image_path(&pose.image);
            let duration_ms = pose
                .duration
                .map(|ticks| ticks.saturating_mul(TICK_MS))
                .filter(|ms| *ms > 0)
                .unwrap_or(DEFAULT_FRAME_MS);
            let frame = decode_frame(dir, &rel, duration_ms, false)?;
            frames.push(frame);
        }
        animations.insert(internal.to_string(), frames);
    }

    if animations.is_empty() {
        return Err(LoaderError::NoAnimations(dir.to_path_buf()));
    }

    Ok(SpriteSheet { animations })
}

/// Normalise un chemin d'image Java vers un chemin relatif au dossier shimeji.
///
/// Les shimejis Java référencent souvent `/walk-1.png` (avec un slash initial)
/// ou `walk-1.png`. On retire un éventuel slash de tête et on préfixe `img/`
/// si le chemin ne contient pas déjà de composant de dossier.
fn normalize_image_path(image: &str) -> String {
    let trimmed = image.trim_start_matches(['/', '\\']);
    if trimmed.contains('/') || trimmed.contains('\\') {
        trimmed.to_string()
    } else {
        format!("img/{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "hyprmeji-java-{}-{}",
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
        fn write_png(&self, name: &str, w: u32, h: u32) {
            let buf = image::RgbaImage::from_pixel(w, h, image::Rgba([0, 255, 0, 255]));
            buf.save(self.0.join("img").join(name)).expect("save png");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn map_names_cover_internal_set() {
        assert_eq!(map_action_name("Stand"), Some("idle"));
        assert_eq!(map_action_name("SitDown"), Some("idle"));
        assert_eq!(map_action_name("Walk"), Some("walk"));
        assert_eq!(map_action_name("Run"), Some("walk"));
        assert_eq!(map_action_name("Falling"), Some("fall"));
        assert_eq!(map_action_name("ClimbWall"), Some("climb"));
        assert_eq!(map_action_name("GrabWall"), Some("climb"));
        assert_eq!(map_action_name("Dragged"), Some("drag"));
        assert_eq!(map_action_name("Bounce"), Some("land"));
        assert_eq!(map_action_name("SomethingWeird"), None);
    }

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize_image_path("/walk-1.png"), "img/walk-1.png");
        assert_eq!(normalize_image_path("walk-1.png"), "img/walk-1.png");
        assert_eq!(normalize_image_path("img/walk-1.png"), "img/walk-1.png");
        assert_eq!(normalize_image_path("\\sub/x.png"), "sub/x.png");
    }

    #[test]
    fn parses_actions_and_maps() {
        let d = TempDir::new("ok");
        d.write_png("stand.png", 5, 5);
        d.write_png("walk1.png", 5, 5);
        d.write_png("walk2.png", 5, 5);

        let xml = r#"
            <Mascot>
              <Action Name="Stand" Type="Stay">
                <Animation>
                  <Pose Image="/stand.png" Duration="10"/>
                </Animation>
              </Action>
              <Action Name="Walk" Type="Move">
                <Animation>
                  <Pose Image="/walk1.png" Duration="6"/>
                  <Pose Image="/walk2.png" Duration="6"/>
                </Animation>
              </Action>
            </Mascot>
        "#;
        let root: ActionsRoot = quick_xml::de::from_str(xml).expect("xml ok");
        let sheet = build_sheet(d.path(), &root).expect("build ok");

        let idle = sheet.animation("idle").expect("idle");
        assert_eq!(idle.len(), 1);
        // 10 ticks * 40 ms = 400 ms.
        assert_eq!(idle[0].duration_ms, 400);

        let walk = sheet.animation("walk").expect("walk");
        assert_eq!(walk.len(), 2);
        assert_eq!(walk[0].duration_ms, 240);
    }

    #[test]
    fn unmappable_action_is_ignored() {
        let d = TempDir::new("ignore");
        d.write_png("s.png", 2, 2);
        let xml = r#"
            <Mascot>
              <Action Name="Stand" Type="Stay">
                <Animation><Pose Image="/s.png" Duration="5"/></Animation>
              </Action>
              <Action Name="ThrowConfetti" Type="Weird">
                <Animation><Pose Image="/s.png" Duration="5"/></Animation>
              </Action>
            </Mascot>
        "#;
        let root: ActionsRoot = quick_xml::de::from_str(xml).expect("xml ok");
        let sheet = build_sheet(d.path(), &root).expect("build ok");
        assert!(sheet.animation("idle").is_some());
        // "ThrowConfetti" n'a pas de cible interne → absent.
        assert_eq!(sheet.animations.len(), 1);
    }

    #[test]
    fn missing_duration_uses_default() {
        let d = TempDir::new("dur");
        d.write_png("s.png", 1, 1);
        let xml = r#"
            <Mascot>
              <Action Name="Stand" Type="Stay">
                <Animation><Pose Image="/s.png"/></Animation>
              </Action>
            </Mascot>
        "#;
        let root: ActionsRoot = quick_xml::de::from_str(xml).expect("xml ok");
        let sheet = build_sheet(d.path(), &root).expect("build ok");
        assert_eq!(
            sheet.animation("idle").expect("idle")[0].duration_ms,
            DEFAULT_FRAME_MS
        );
    }

    #[test]
    fn no_mappable_actions_is_error() {
        let d = TempDir::new("err");
        d.write_png("s.png", 1, 1);
        let xml = r#"
            <Mascot>
              <Action Name="Whatever" Type="X">
                <Animation><Pose Image="/s.png" Duration="5"/></Animation>
              </Action>
            </Mascot>
        "#;
        let root: ActionsRoot = quick_xml::de::from_str(xml).expect("xml ok");
        assert!(matches!(
            build_sheet(d.path(), &root),
            Err(LoaderError::NoAnimations(_))
        ));
    }

    #[test]
    fn duplicate_internal_target_keeps_first() {
        let d = TempDir::new("dup");
        d.write_png("a.png", 1, 1);
        d.write_png("b.png", 2, 2);
        let xml = r#"
            <Mascot>
              <Action Name="Walk" Type="Move">
                <Animation><Pose Image="/a.png" Duration="3"/></Animation>
              </Action>
              <Action Name="Run" Type="Move">
                <Animation><Pose Image="/b.png" Duration="3"/></Animation>
              </Action>
            </Mascot>
        "#;
        let root: ActionsRoot = quick_xml::de::from_str(xml).expect("xml ok");
        let sheet = build_sheet(d.path(), &root).expect("build ok");
        let walk = sheet.animation("walk").expect("walk");
        // La première action "Walk" (a.png, 1x1) est conservée.
        assert_eq!((walk[0].width, walk[0].height), (1, 1));
    }
}