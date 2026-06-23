// crates/hyprmeji-core/src/types.rs
//! Types partagés du crate `hyprmeji-core`.
//!
//! Aucune dépendance Wayland, I/O ou filesystem : ces types sont de purs
//! conteneurs de données manipulés par la machine d'états, la physique et le
//! lecteur d'animations.

use std::collections::HashMap;
use std::sync::Arc;

/// Vecteur 2D (position ou vélocité), en pixels ou px/s selon le contexte.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    /// Vecteur nul.
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    /// Construit un vecteur à partir de ses composantes.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Norme euclidienne du vecteur.
    #[must_use]
    pub fn length(self) -> f32 {
        self.x.hypot(self.y)
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl std::ops::Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, rhs: f32) -> Vec2 {
        Vec2::new(self.x * rhs, self.y * rhs)
    }
}

impl std::ops::AddAssign for Vec2 {
    fn add_assign(&mut self, rhs: Vec2) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

/// Rectangle aligné sur les axes (coin haut-gauche + dimensions).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Construit un rectangle.
    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Indique si un point est contenu dans le rectangle (bords inclus).
    #[must_use]
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }

    /// Indique si deux rectangles se chevauchent.
    #[must_use]
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && self.x + self.w > other.x
            && self.y < other.y + other.h
            && self.y + self.h > other.y
    }
}

/// Une frame d'animation : pixels RGBA pré-décodés.
///
/// Les pixels sont partagés via `Arc` pour permettre un clonage bon marché des
/// `SpriteSheet` et des frames sans recopier les buffers.
#[derive(Debug, Clone)]
pub struct AnimationFrame {
    /// Données RGBA, row-major (`width * height * 4` octets).
    pub pixels: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// Durée d'affichage de la frame, en millisecondes.
    pub duration_ms: u32,
    /// Si `true`, la frame doit être affichée en miroir horizontal.
    pub flip_x: bool,
}

/// Ensemble des animations d'un shimeji, indexées par nom.
#[derive(Debug, Clone, Default)]
pub struct SpriteSheet {
    pub animations: HashMap<String, Vec<AnimationFrame>>,
}

impl SpriteSheet {
    /// Crée une feuille de sprites vide.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Retourne les frames d'une animation donnée, si elle existe.
    #[must_use]
    pub fn animation(&self, name: &str) -> Option<&[AnimationFrame]> {
        self.animations.get(name).map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec2_arithmetic() {
        let a = Vec2::new(1.0, 2.0);
        let b = Vec2::new(3.0, 4.0);
        assert_eq!(a + b, Vec2::new(4.0, 6.0));
        assert_eq!(b - a, Vec2::new(2.0, 2.0));
        assert_eq!(a * 2.0, Vec2::new(2.0, 4.0));
    }

    #[test]
    fn vec2_add_assign() {
        let mut a = Vec2::new(1.0, 1.0);
        a += Vec2::new(2.0, 3.0);
        assert_eq!(a, Vec2::new(3.0, 4.0));
    }

    #[test]
    fn vec2_length() {
        assert!((Vec2::new(3.0, 4.0).length() - 5.0).abs() < f32::EPSILON);
        assert_eq!(Vec2::ZERO.length(), 0.0);
    }

    #[test]
    fn rect_contains() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Vec2::new(5.0, 5.0)));
        assert!(r.contains(Vec2::new(0.0, 0.0)));
        assert!(r.contains(Vec2::new(10.0, 10.0)));
        assert!(!r.contains(Vec2::new(11.0, 5.0)));
        assert!(!r.contains(Vec2::new(-1.0, 5.0)));
    }

    #[test]
    fn rect_intersects() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        let c = Rect::new(20.0, 20.0, 5.0, 5.0);
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
        assert!(!a.intersects(&c));
    }

    #[test]
    fn sprite_sheet_lookup() {
        let mut sheet = SpriteSheet::new();
        let frame = AnimationFrame {
            pixels: Arc::from(vec![0u8; 4].into_boxed_slice()),
            width: 1,
            height: 1,
            duration_ms: 100,
            flip_x: false,
        };
        sheet.animations.insert("idle".to_string(), vec![frame]);
        assert!(sheet.animation("idle").is_some());
        assert_eq!(sheet.animation("idle").map(<[_]>::len), Some(1));
        assert!(sheet.animation("walk").is_none());
    }
}