// crates/hyprmeji-render/src/buffer.rs
//! Allocation de buffers `wl_shm` (double buffering) et transformations pixel
//! pures.
//!
//! Ce module sépare strictement deux responsabilités :
//!
//! 1. **Logique pixel pure** (sans Wayland) : conversion RGBA → ARGB8888,
//!    flip horizontal d'un buffer RGBA, calcul des marges depuis une position.
//!    Ces fonctions sont testables unitairement sans compositor (voir `tests`).
//! 2. **Pool de buffers `wl_shm`** : [`BufferPool`] gère deux slots alternés en
//!    mémoire partagée et n'écrit jamais dans un slot encore détenu par le
//!    compositor (libération signalée par l'événement `wl_buffer::release`).
//!
//! Format mémoire des buffers : **ARGB8888**, qui en little-endian se présente
//! comme la séquence d'octets `[B, G, R, A]` par pixel — le format natif de
//! `wl_shm` (`Format::Argb8888`) et de cairo (`Format::ARgb32`).

use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};

use crate::error::RenderError;

/// Nombre d'octets par pixel (RGBA et ARGB8888 sont tous deux 4 octets).
pub(crate) const BYTES_PER_PIXEL: usize = 4;

/// Convertit un buffer RGBA (row-major, `[R, G, B, A]`) en ARGB8888 mémoire
/// (`[B, G, R, A]` en little-endian), en place dans `dst`.
///
/// `src` et `dst` doivent avoir la même longueur, multiple de 4. La pré-
/// multiplication alpha n'est pas appliquée : les sprites sont supposés fournis
/// en alpha droit, ce qui convient au compositing du compositor pour un overlay
/// simple.
///
/// # Panics
/// Ne panique pas : si les longueurs diffèrent, la fonction ne traite que le
/// préfixe commun (les appelants garantissent l'égalité en amont).
pub(crate) fn rgba_to_argb8888_into(src: &[u8], dst: &mut [u8]) {
    let n = src.len().min(dst.len()) / BYTES_PER_PIXEL * BYTES_PER_PIXEL;
    let mut i = 0;
    while i < n {
        let r = src[i];
        let g = src[i + 1];
        let b = src[i + 2];
        let a = src[i + 3];
        dst[i] = b;
        dst[i + 1] = g;
        dst[i + 2] = r;
        dst[i + 3] = a;
        i += BYTES_PER_PIXEL;
    }
}

/// Retourne une copie d'un buffer RGBA miroir horizontal.
///
/// Les pixels de chaque ligne sont inversés gauche↔droite. `width * height * 4`
/// doit correspondre à `rgba.len()` ; sinon une copie inchangée est renvoyée
/// (cas défensif, non attendu en production).
#[must_use]
pub(crate) fn flip_horizontal_rgba(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let stride = w * BYTES_PER_PIXEL;
    if stride * h != rgba.len() || w == 0 || h == 0 {
        return rgba.to_vec();
    }
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        let row = y * stride;
        for x in 0..w {
            let sx = w - 1 - x;
            let si = row + sx * BYTES_PER_PIXEL;
            let di = row + x * BYTES_PER_PIXEL;
            out[di..di + BYTES_PER_PIXEL].copy_from_slice(&rgba[si..si + BYTES_PER_PIXEL]);
        }
    }
    out
}

/// Marges `(top, right, bottom, left)` d'une layer surface positionnée
/// librement par son coin haut-gauche en `(x, y)`.
///
/// La surface est ancrée en haut-gauche ; seules les marges `top` et `left`
/// portent la position, `right`/`bottom` restent nuls. Les positions négatives
/// sont bornées à 0 (la surface ne sort pas par le haut/gauche de l'écran).
#[must_use]
pub(crate) fn margins_for(x: f32, y: f32) -> (i32, i32, i32, i32) {
    let left = x.max(0.0).round() as i32;
    let top = y.max(0.0).round() as i32;
    (top, 0, 0, left)
}

/// Pool de deux buffers `wl_shm` alternés (double buffering).
///
/// S'appuie sur le [`SlotPool`] de smithay-client-toolkit, qui gère le suivi de
/// possession : un slot encore détenu par le compositor n'est pas réutilisé, ce
/// qui garantit qu'on n'écrit jamais dans un buffer en cours d'affichage.
pub(crate) struct BufferPool {
    pool: SlotPool,
    width: u32,
    height: u32,
}

impl BufferPool {
    /// Crée un pool dimensionné pour `width * height` pixels ARGB8888.
    ///
    /// La capacité initiale couvre deux buffers afin de permettre l'alternance
    /// sans réallocation tant que la taille du sprite ne change pas.
    pub(crate) fn new(
        shm: &smithay_client_toolkit::shm::Shm,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        let len = Self::byte_len(width, height);
        let pool = SlotPool::new(len.saturating_mul(2).max(len.max(1)), shm)
            .map_err(|e| RenderError::BufferAlloc(e.to_string()))?;
        Ok(Self {
            pool,
            width,
            height,
        })
    }

    /// Taille en octets d'un buffer `width * height` en ARGB8888.
    #[must_use]
    pub(crate) fn byte_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * BYTES_PER_PIXEL
    }

    /// Redimensionne le pool si le sprite courant a changé de taille.
    pub(crate) fn ensure_size(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if width == self.width && height == self.height {
            return Ok(());
        }
        let len = Self::byte_len(width, height).saturating_mul(2).max(1);
        self.pool
            .resize(len)
            .map_err(|e| RenderError::BufferAlloc(e.to_string()))?;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Acquiert un buffer libre et fournit son canvas pour écriture.
    ///
    /// `SlotPool::create_buffer` renvoie un slot non détenu par le compositor
    /// (ou en crée un), réalisant l'alternance double-buffer attendue. Le canvas
    /// retourné est la zone mémoire ARGB8888 à remplir, puis à attacher.
    pub(crate) fn acquire(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<(Buffer, &mut [u8]), RenderError> {
        let stride = width as i32 * BYTES_PER_PIXEL as i32;
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .map_err(|e| RenderError::BufferAlloc(e.to_string()))?;
        Ok((buffer, canvas))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_to_argb_swizzles_channels() {
        // 2 pixels : (R,G,B,A) = (10,20,30,40) puis (50,60,70,80).
        let src = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let mut dst = [0u8; 8];
        rgba_to_argb8888_into(&src, &mut dst);
        // ARGB8888 little-endian = [B, G, R, A].
        assert_eq!(dst, [30, 20, 10, 40, 70, 60, 50, 80]);
    }

    #[test]
    fn rgba_to_argb_preserves_alpha() {
        let src = [1u8, 2, 3, 0, 9, 9, 9, 255];
        let mut dst = [0u8; 8];
        rgba_to_argb8888_into(&src, &mut dst);
        assert_eq!(dst[3], 0);
        assert_eq!(dst[7], 255);
    }

    #[test]
    fn rgba_to_argb_handles_length_mismatch_gracefully() {
        let src = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let mut dst = [0u8; 4]; // une seule place pixel
        rgba_to_argb8888_into(&src, &mut dst);
        assert_eq!(dst, [30, 20, 10, 40]);
    }

    #[test]
    fn flip_horizontal_reverses_each_row() {
        // 2x1 : px0 puis px1 → après flip : px1 puis px0.
        let src = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let out = flip_horizontal_rgba(&src, 2, 1);
        assert_eq!(out, [50, 60, 70, 80, 10, 20, 30, 40]);
    }

    #[test]
    fn flip_horizontal_two_rows_independent() {
        // 2x2, lignes distinctes : flip chaque ligne séparément.
        // ligne 0 : A B   ligne 1 : C D
        let a = [1u8, 1, 1, 1];
        let b = [2u8, 2, 2, 2];
        let c = [3u8, 3, 3, 3];
        let d = [4u8, 4, 4, 4];
        let mut src = Vec::new();
        src.extend_from_slice(&a);
        src.extend_from_slice(&b);
        src.extend_from_slice(&c);
        src.extend_from_slice(&d);
        let out = flip_horizontal_rgba(&src, 2, 2);
        let mut expected = Vec::new();
        expected.extend_from_slice(&b);
        expected.extend_from_slice(&a);
        expected.extend_from_slice(&d);
        expected.extend_from_slice(&c);
        assert_eq!(out, expected);
    }

    #[test]
    fn flip_horizontal_single_column_is_identity() {
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8]; // 1x2
        let out = flip_horizontal_rgba(&src, 1, 2);
        assert_eq!(out, src);
    }

    #[test]
    fn flip_horizontal_bad_dimensions_returns_copy() {
        let src = [1u8, 2, 3, 4]; // 1 pixel mais on annonce 2x2
        let out = flip_horizontal_rgba(&src, 2, 2);
        assert_eq!(out, src);
    }

    #[test]
    fn margins_from_position() {
        // (top, right, bottom, left)
        assert_eq!(margins_for(100.0, 200.0), (200, 0, 0, 100));
        assert_eq!(margins_for(0.0, 0.0), (0, 0, 0, 0));
    }

    #[test]
    fn margins_clamp_negative_to_zero() {
        assert_eq!(margins_for(-50.0, -10.0), (0, 0, 0, 0));
        assert_eq!(margins_for(-5.0, 30.0), (30, 0, 0, 0));
    }

    #[test]
    fn margins_round_fractional_position() {
        assert_eq!(margins_for(10.4, 20.6), (21, 0, 0, 10));
    }

    #[test]
    fn byte_len_is_width_times_height_times_four() {
        assert_eq!(BufferPool::byte_len(128, 128), 128 * 128 * 4);
        assert_eq!(BufferPool::byte_len(0, 10), 0);
    }
}