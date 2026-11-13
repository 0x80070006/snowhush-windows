//! Construction de la liste de sprites d'une image (pur CPU, testable).
//!
//! Ordre de dessin (= profondeur) : flocons lointains, tas (corps puis bord
//! adouci), reflets scintillants, flocons principaux, flocons flous au
//! premier plan.

use crate::sim::{Sim, LAYER_FAR, LAYER_MID, LAYER_NEAR};
use std::f32::consts::PI;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F, D2D_RECT_U};

pub const ATLAS_W: u32 = 128;
pub const ATLAS_H: u32 = 64;
/// Flocon net (disque doux).
const CRISP: D2D_RECT_U = D2D_RECT_U { left: 0, top: 0, right: 32, bottom: 32 };
/// Flocon flou (premier plan, effet bokeh).
const SOFT: D2D_RECT_U = D2D_RECT_U { left: 32, top: 0, right: 64, bottom: 32 };
/// Reflet : cœur brillant et petite croix.
const STAR: D2D_RECT_U = D2D_RECT_U { left: 0, top: 32, right: 32, bottom: 64 };
/// Corps du tas (dégradé blanc vers bleu glacier).
const BODY: D2D_RECT_U = D2D_RECT_U { left: 72, top: 2, right: 76, bottom: 62 };
/// Bord supérieur du tas : transparent vers opaque (anti-crénelage).
const CAP: D2D_RECT_U = D2D_RECT_U { left: 88, top: 0, right: 92, bottom: 64 };

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Pixels BGRA prémultipliés de l'atlas.
pub fn atlas_pixels() -> Vec<u8> {
    let w = ATLAS_W;
    let mut px = vec![0u8; (w * ATLAS_H * 4) as usize];
    let mut put = |x: u32, y: u32, r: f32, g: f32, b: f32, a: f32| {
        let i = ((y * w + x) * 4) as usize;
        let a = a.clamp(0.0, 1.0);
        px[i] = (b * a * 255.0) as u8;
        px[i + 1] = (g * a * 255.0) as u8;
        px[i + 2] = (r * a * 255.0) as u8;
        px[i + 3] = (a * 255.0) as u8;
    };
    for y in 0..32u32 {
        for x in 0..32u32 {
            let dx = x as f32 + 0.5 - 16.0;
            let dy = y as f32 + 0.5 - 16.0;
            let d = (dx * dx + dy * dy).sqrt() / 16.0;
            // Net : cœur plein, bord doux.
            put(x, y, 1.0, 1.0, 1.0, smoothstep(1.0, 0.38, d));
            // Flou : gaussienne qui s'annule au bord.
            let g = ((-4.0 * d * d).exp() - (-4.0f32).exp()) / (1.0 - (-4.0f32).exp());
            put(x + 32, y, 1.0, 1.0, 1.0, g.max(0.0));
            // Reflet : cœur + croix fine.
            let core = smoothstep(0.35, 0.0, d);
            let cross = (smoothstep(0.09, 0.0, dx.abs() / 16.0) + smoothstep(0.09, 0.0, dy.abs() / 16.0)) * smoothstep(1.0, 0.1, d) * 0.55;
            put(x, y + 32, 1.0, 1.0, 1.0, (core + cross).min(1.0));
        }
    }
    for y in 0..ATLAS_H {
        let f = y as f32 / (ATLAS_H - 1) as f32;
        // Corps : blanc légèrement chaud en surface, ombre bleutée en profondeur.
        let k = f.powf(0.7);
        for x in 70..78u32 {
            put(x, y, 1.0 - 0.12 * k, 1.0 - 0.07 * k, 1.0 - 0.01 * k, 1.0);
        }
        // Bord : fondu sur toute la hauteur.
        let a = smoothstep(0.0, 1.0, f);
        for x in 86..94u32 {
            put(x, y, 1.0, 1.0, 1.0, a);
        }
    }
    px
}

#[inline]
fn hash(a: u32, b: u32) -> u32 {
    let mut x = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77).rotate_left(13);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^ (x >> 15)
}

#[inline]
fn hf(a: u32, b: u32) -> f32 {
    (hash(a, b) >> 8) as f32 * (1.0 / 16_777_216.0)
}

#[derive(Default)]
pub struct Sprites {
    pub rects: Vec<D2D_RECT_F>,
    pub srcs: Vec<D2D_RECT_U>,
    pub colors: Vec<D2D1_COLOR_F>,
    hd: Vec<f32>,
}

struct View {
    w: f32,
    h: f32,
}

impl Sprites {
    #[inline]
    fn push(&mut self, r: D2D_RECT_F, src: D2D_RECT_U, a: f32) {
        self.rects.push(r);
        self.srcs.push(src);
        self.colors.push(D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a });
    }

    fn flakes(&mut self, sim: &Sim, v: &View, layer: u8, opacity: f32) {
        let (src, k) = if layer == LAYER_NEAR { (SOFT, 1.8) } else { (CRISP, 1.7) };
        for p in sim.particles.iter().filter(|p| p.layer == layer) {
            let r = p.r * k;
            if p.x + r < 0.0 || p.x - r > v.w || p.y + r < 0.0 || p.y - r > v.h {
                continue;
            }
            self.push(D2D_RECT_F { left: p.x - r, top: p.y - r, right: p.x + r, bottom: p.y + r }, src, p.alpha * opacity);
        }
    }

    /// Un tas : hauteurs lissées pour l'affichage, interpolées entre les
    /// centres de colonnes (demi-colonnes), avec un bord supérieur fondu.
    #[allow(clippy::too_many_arguments)]
    fn pile(
        &mut self,
        v: &View,
        h: &[f32],
        x0: f32,
        bin: f32,
        open: impl Fn(usize) -> bool,
        linked: impl Fn(usize) -> bool,
        base: impl Fn(usize) -> f32,
        taper: bool,
        e: f32,
        alpha: f32,
    ) {
        let n = h.len();
        if n == 0 || x0 > v.w || x0 + n as f32 * bin < 0.0 {
            return;
        }
        let mut hd = std::mem::take(&mut self.hd);
        hd.clear();
        hd.extend((0..n).map(|i| {
            if !open(i) {
                return 0.0;
            }
            let wall = if taper { 0.0 } else { h[i] };
            let l = if i > 0 && linked(i - 1) { h[i - 1] } else { wall };
            let r = if i + 1 < n && linked(i) { h[i + 1] } else { wall };
            (l + 2.0 * h[i] + r) * 0.25
        }));
        let half = bin * 0.5;
        for i in 0..n {
            if !open(i) {
                continue;
            }
            let x = x0 + i as f32 * bin;
            if x + bin < 0.0 || x > v.w {
                continue;
            }
            let hi = hd[i];
            let wall = if taper { 0.0 } else { hi };
            let hp = if i > 0 && linked(i - 1) { hd[i - 1] } else { wall };
            let hn = if i + 1 < n && linked(i) { hd[i + 1] } else { wall };
            let b = base(i);
            self.column(x, half, hp * 0.25 + hi * 0.75, b, e, alpha);
            self.column(x + half, half, hi * 0.75 + hn * 0.25, b, e, alpha);
        }
        self.hd = hd;
    }

    #[inline]
    fn column(&mut self, x: f32, w: f32, v: f32, base: f32, e: f32, alpha: f32) {
        if v < 0.35 {
            return;
        }
        let top = base - v;
        // Fine pellicule : un voile un peu transparent, comme une vraie poudrée.
        let a = alpha * (0.45 + 0.55 * (v / (4.0 * e))).min(1.0);
        if top + e < base {
            self.push(D2D_RECT_F { left: x, top: top + e, right: x + w, bottom: base }, BODY, a);
        }
        self.push(D2D_RECT_F { left: x, top: top - e, right: x + w, bottom: (top + e).min(base) }, CAP, a);
    }

    /// Petits reflets qui s'allument et s'éteignent à la surface des tas.
    fn sparkles(&mut self, sim: &Sim, v: &View, opacity: f32) {
        let s = sim.scale;
        let t = sim.time();
        let slots = ((v.w / (60.0 * s)) as u32).clamp(4, 96);
        let g = &sim.ground;
        for k in 0..slots {
            let period = 1.4 + 2.6 * hf(k, 1);
            let tt = t + period * hf(k, 2);
            let cyc = (tt / period).floor();
            let ph = tt / period - cyc;
            let life = 0.14;
            if ph > life {
                continue;
            }
            let env = (PI * ph / life).sin();
            let hs = hash(k, cyc as u32);
            let pick = (hs >> 8) as usize;
            let pos = if !sim.surfaces.is_empty() && hs & 3 == 0 {
                let sf = &sim.surfaces[pick % sim.surfaces.len()];
                let b = (pick >> 4) % sf.h.len().max(1);
                (sf.h.get(b).copied().unwrap_or(0.0) > 2.5 && sf.open[b]).then(|| (sf.left + (b as f32 + 0.5) * sim.bin, sf.top - sf.h[b]))
            } else if !g.h.is_empty() {
                let b = pick % g.h.len();
                (g.h[b] > 2.5 && g.base[b].is_finite()).then(|| ((b as f32 + 0.5) * sim.bin, g.base[b] - g.h[b]))
            } else {
                None
            };
            let Some((x, y)) = pos else { continue };
            let y = y + s * (0.8 + 2.0 * hf(hs, 3));
            let r = s * (1.8 + 2.2 * hf(hs, 4));
            self.push(D2D_RECT_F { left: x - r, top: y - r, right: x + r, bottom: y + r }, STAR, env * 0.95 * opacity);
        }
    }

    pub fn build(&mut self, sim: &Sim, w: f32, h: f32, opacity: f32) {
        self.rects.clear();
        self.srcs.clear();
        self.colors.clear();
        let v = View { w, h };
        let e = sim.scale.max(1.0);
        let bin = sim.bin;

        self.flakes(sim, &v, LAYER_FAR, opacity);

        let pile_a = 0.97 * opacity;
        let g = &sim.ground;
        self.pile(
            &v,
            &g.h,
            0.0,
            bin,
            |i| g.base[i].is_finite(),
            |i| i + 1 < g.base.len() && g.base[i].is_finite() && g.base[i + 1].is_finite() && (g.base[i] - g.base[i + 1]).abs() < 1.0,
            |i| g.base[i],
            false,
            e,
            pile_a,
        );
        for sf in &sim.surfaces {
            let open = &sf.open;
            self.pile(
                &v,
                &sf.h,
                sf.left,
                bin,
                |i| open[i],
                |i| i + 1 < open.len() && open[i] && open[i + 1],
                |_| sf.top,
                true,
                e,
                pile_a,
            );
        }
        self.sparkles(sim, &v, opacity);
        self.flakes(sim, &v, LAYER_MID, opacity);
        self.flakes(sim, &v, LAYER_NEAR, opacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Monitor, Params, Rect, WinTop};

    #[test]
    fn atlas_is_premultiplied() {
        let px = atlas_pixels();
        assert_eq!(px.len(), (ATLAS_W * ATLAS_H * 4) as usize);
        for c in px.chunks(4) {
            assert!(c[0] <= c[3] && c[1] <= c[3] && c[2] <= c[3]);
        }
    }

    #[test]
    fn build_is_consistent_and_finite() {
        let mon = [Monitor {
            rect: Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1080.0 },
            work: Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1032.0 },
        }];
        let mut sim = Sim::new(1920.0, 1080.0, 1.25, &mon, false, 9);
        let w = WinTop { id: 1, left: 200.0, right: 1000.0, top: 300.0, vis: vec![(200.0, 1000.0)] };
        let p = Params { rate: 28.0, ..Params::default() };
        for _ in 0..1800 {
            sim.sync_windows(std::slice::from_ref(&w));
            sim.update(1.0 / 60.0, &p);
        }
        let mut sp = Sprites::default();
        sp.build(&sim, 1920.0, 1080.0, 0.85);
        assert!(!sp.rects.is_empty());
        assert_eq!(sp.rects.len(), sp.srcs.len());
        assert_eq!(sp.rects.len(), sp.colors.len());
        for (r, c) in sp.rects.iter().zip(&sp.colors) {
            assert!(r.left.is_finite() && r.top.is_finite() && r.right.is_finite() && r.bottom.is_finite());
            assert!(r.right >= r.left && r.bottom >= r.top);
            assert!((0.0..=1.0).contains(&c.a));
        }
    }
}
