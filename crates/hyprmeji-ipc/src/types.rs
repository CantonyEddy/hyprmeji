// crates/hyprmeji-ipc/src/types.rs
//! Types de données IPC.
//!
//! [`WindowInfo`] est la forme normalisée d'une fenêtre Hyprland telle que
//! consommée par le reste de hyprmeji. Hyprland expose la géométrie sous forme
//! de tableaux (`at: [x, y]`, `size: [w, h]`) via `j/clients` ; la
//! désérialisation passe donc par une structure intermédiaire [`RawClient`]
//! avant d'être aplatie en `WindowInfo`.

use serde::Deserialize;

/// Information sur une fenêtre Hyprland, normalisée pour hyprmeji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Adresse unique de la fenêtre (ex. `0x55a...`).
    pub address: String,
    /// Titre de la fenêtre.
    pub title: String,
    /// Classe applicative (`class`).
    pub class: String,
    /// Position X du coin haut-gauche, en pixels.
    pub x: i32,
    /// Position Y du coin haut-gauche, en pixels.
    pub y: i32,
    /// Largeur, en pixels.
    pub width: u32,
    /// Hauteur, en pixels.
    pub height: u32,
}

/// Forme brute d'un client telle que renvoyée par `hyprctl clients -j`
/// (`j/clients` sur le socket). Champs non utilisés ignorés.
#[derive(Debug, Deserialize)]
pub(crate) struct RawClient {
    #[serde(default)]
    address: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    class: String,
    /// Position `[x, y]`.
    #[serde(default)]
    at: [i32; 2],
    /// Dimensions `[width, height]`.
    #[serde(default)]
    size: [i32; 2],
}

impl From<RawClient> for WindowInfo {
    fn from(raw: RawClient) -> Self {
        WindowInfo {
            address: raw.address,
            title: raw.title,
            class: raw.class,
            x: raw.at[0],
            y: raw.at[1],
            // Les dimensions Hyprland sont positives ; on borne à 0 par
            // prudence pour éviter un cast négatif silencieux.
            width: raw.size[0].max(0) as u32,
            height: raw.size[1].max(0) as u32,
        }
    }
}

/// Désérialise la réponse JSON de `j/clients` en `Vec<WindowInfo>`.
pub(crate) fn parse_clients(json: &str) -> Result<Vec<WindowInfo>, serde_json::Error> {
    let raw: Vec<RawClient> = serde_json::from_str(json)?;
    Ok(raw.into_iter().map(WindowInfo::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
    [
      {
        "address": "0x55a1",
        "title": "Alacritty",
        "class": "Alacritty",
        "at": [100, 200],
        "size": [800, 600]
      },
      {
        "address": "0x55a2",
        "title": "Firefox",
        "class": "firefox",
        "at": [0, 0],
        "size": [1920, 1040]
      }
    ]
    "#;

    #[test]
    fn parses_clients_array() {
        let windows = parse_clients(SAMPLE).expect("parse ok");
        assert_eq!(windows.len(), 2);

        let a = &windows[0];
        assert_eq!(a.address, "0x55a1");
        assert_eq!(a.title, "Alacritty");
        assert_eq!(a.class, "Alacritty");
        assert_eq!(a.x, 100);
        assert_eq!(a.y, 200);
        assert_eq!(a.width, 800);
        assert_eq!(a.height, 600);

        let b = &windows[1];
        assert_eq!(b.x, 0);
        assert_eq!(b.width, 1920);
        assert_eq!(b.height, 1040);
    }

    #[test]
    fn empty_array_yields_empty_vec() {
        let windows = parse_clients("[]").expect("parse ok");
        assert!(windows.is_empty());
    }

    #[test]
    fn missing_fields_use_defaults() {
        let json = r#"[{ "address": "0x1" }]"#;
        let windows = parse_clients(json).expect("parse ok");
        assert_eq!(windows.len(), 1);
        let w = &windows[0];
        assert_eq!(w.address, "0x1");
        assert_eq!(w.title, "");
        assert_eq!(w.x, 0);
        assert_eq!(w.width, 0);
    }

    #[test]
    fn negative_size_is_clamped_to_zero() {
        let json = r#"[{ "address": "0x1", "at": [10, 10], "size": [-5, 20] }]"#;
        let windows = parse_clients(json).expect("parse ok");
        assert_eq!(windows[0].width, 0);
        assert_eq!(windows[0].height, 20);
    }

    #[test]
    fn invalid_json_is_error() {
        assert!(parse_clients("{ not an array").is_err());
    }
}