//! Simulation de la neige : indépendante de la plateforme, sans allocation
//! dans la boucle chaude (hors création de particules, bornée par MAX_PARTICLES).
//!
//! Unités : pixels physiques de l'overlay (origine = coin haut-gauche de
//! l'écran virtuel). `scale` = DPI / 96.

use std::f32::consts::{PI, TAU};

/// Plafond dur du nombre de particules (protège CPU et mémoire).
pub const MAX_PARTICLES: usize = 6000;
/// Plafond de surfaces (fenêtres) suivies simultanément.
pub const MAX_SURFACES: usize = 48;
/// Fraction du volume d'un flocon qui s'ajoute au tas (tassement).
const PACKING: f32 = 0.55;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Monitor {
    pub rect: Rect,
    pub work: Rect,
}

/// Bord supérieur d'une fenêtre visible, fourni par le tracker.
#[derive(Clone, Debug, Default)]
pub struct WinTop {
    pub id: isize,
    pub left: f32,
    pub right: f32,
    pub top: f32,
    /// Intervalles [a, b) du bord réellement visibles (non recouverts).
    pub vis: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Flocons / seconde pour 1000 px logiques de largeur.
    pub rate: f32,
    pub ground: bool,
    pub windows: bool,
    pub melt: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self { rate: 7.0, ground: true, windows: true, melt: true }
    }
}

/// Générateur xorshift64* : rapide, suffisant pour du visuel.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    #[inline]
    pub fn f(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }
    #[inline]
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub x: f32,
    pub y: f32,
    /// Impulsion (curseur, éjection) qui s'amortit.
    pub vx: f32,
    pub vy: f32,
    pub r: f32,
    pub alpha: f32,
    /// Plan : LAYER_FAR (lointain, ne se pose pas), LAYER_MID (interagit),
    /// LAYER_NEAR (premier plan flou, passe devant).
    pub layer: u8,
    fall: f32,
    phase: f32,
    freq: f32,
    amp: f32,
    drift: f32,
    vol: f32,
    lump: bool,
}

pub const LAYER_FAR: u8 = 0;
pub const LAYER_MID: u8 = 1;
pub const LAYER_NEAR: u8 = 2;
/// Part des flocons du plan principal (les autres sont décoratifs).
const MID_SHARE: f32 = 0.70;

/// Tas de neige au sol (une colonne par "bin").
pub struct Ground {
    /// Y du sol par colonne (NaN = pas d'écran sous cette colonne).
    pub base: Vec<f32>,
    pub h: Vec<f32>,
    cap: Vec<f32>,
    /// link[i] : la colonne i est reliée à i+1 (même sol).
    link: Vec<bool>,
}

/// Tas de neige posé sur le haut d'une fenêtre.
pub struct Surface {
    pub id: isize,
    pub left: f32,
    pub top: f32,
    pub h: Vec<f32>,
    pub open: Vec<bool>,
    spill_l: f32,
    spill_r: f32,
    seen: bool,
}

enum Hit {
    Ground(usize),
    Surface(usize, usize),
    Kill,
}

pub struct Sim {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    /// Largeur d'une colonne de tas, en px.
    pub bin: f32,
    pub particles: Vec<Particle>,
    pub ground: Ground,
    pub surfaces: Vec<Surface>,
    rng: Rng,
    time: f32,
    wind: f32,
    spawn_acc: f32,
    spray_acc: f32,
    cursor: Option<(f32, f32)>,
    prev_cursor: Option<(f32, f32)>,
}

impl Sim {
    pub fn new(width: f32, height: f32, scale: f32, monitors: &[Monitor], behind_taskbar: bool, seed: u64) -> Self {
        let scale = scale.clamp(0.5, 4.0);
        let mut s = Sim {
            width,
            height,
            scale,
            bin: (2.0 * scale).round().max(2.0),
            particles: Vec::with_capacity(1024),
            ground: Ground { base: Vec::new(), h: Vec::new(), cap: Vec::new(), link: Vec::new() },
            surfaces: Vec::new(),
            rng: Rng::new(seed),
            time: 0.0,
            wind: 0.0,
            spawn_acc: 0.0,
            spray_acc: 0.0,
            cursor: None,
            prev_cursor: None,
        };
        s.set_geometry(width, height, monitors, behind_taskbar);
        s
    }

    /// (Re)calcule le sol après un changement d'affichage. Conserve la neige
    /// si la largeur n'a pas changé.
    pub fn set_geometry(&mut self, width: f32, height: f32, monitors: &[Monitor], behind_taskbar: bool) {
        let n = ((width / self.bin).ceil() as usize).max(1);
        let keep = n == self.ground.h.len() && (width - self.width).abs() < 0.5;
        self.width = width;
        self.height = height;
        let mut base = vec![f32::NAN; n];
        let mut cap = vec![0.0; n];
        for (i, b) in base.iter_mut().enumerate() {
            let x = (i as f32 + 0.5) * self.bin;
            if let Some(m) = monitors.iter().find(|m| x >= m.rect.l && x < m.rect.r) {
                let y = if behind_taskbar { m.rect.b } else { m.work.b };
                *b = y;
                cap[i] = ((m.rect.b - m.rect.t) * 0.16).min(170.0 * self.scale);
            }
        }
        let link = (0..n)
            .map(|i| i + 1 < n && base[i].is_finite() && base[i + 1].is_finite() && (base[i] - base[i + 1]).abs() < 1.0)
            .collect();
        let h = if keep {
            let mut h = std::mem::take(&mut self.ground.h);
            for (v, c) in h.iter_mut().zip(&cap) {
                *v = v.min(*c);
            }
            h
        } else {
            vec![0.0; n]
        };
        self.ground = Ground { base, h, cap, link };
        // Les fenêtres seront resynchronisées au prochain scan.
        self.surfaces.clear();
        self.particles.retain(|p| p.x > -width && p.x < 2.0 * width && p.y < height + 50.0);
    }

    pub fn time(&self) -> f32 {
        self.time
    }

    pub fn set_cursor(&mut self, c: Option<(f32, f32)>) {
        self.cursor = c;
    }

    /// Vide tous les tas.
    pub fn clear(&mut self) {
        self.ground.h.iter_mut().for_each(|h| *h = 0.0);
        for s in &mut self.surfaces {
            s.h.iter_mut().for_each(|h| *h = 0.0);
        }
        self.particles.retain(|p| !p.lump);
    }

    /// Fait tomber la neige de toutes les fenêtres.
    pub fn drop_windows(&mut self) {
        let surfaces = std::mem::take(&mut self.surfaces);
        for s in &surfaces {
            self.drop_bins(s.left, s.top, &s.h, 0..s.h.len());
        }
    }

    pub fn clear_ground(&mut self) {
        self.ground.h.iter_mut().for_each(|h| *h = 0.0);
    }

    #[cfg(test)]
    pub fn total_snow(&self) -> f32 {
        let g: f32 = self.ground.h.iter().sum();
        let s: f32 = self.surfaces.iter().flat_map(|s| s.h.iter()).sum();
        (g + s) * self.bin
    }

    /// Synchronise les surfaces avec les fenêtres visibles (ordre z, haut d'abord).
    pub fn sync_windows(&mut self, wins: &[WinTop]) {
        let mut surfaces = std::mem::take(&mut self.surfaces);
        for s in &mut surfaces {
            s.seen = false;
        }
        for w in wins.iter().take(MAX_SURFACES) {
            let n = (((w.right - w.left) / self.bin).floor() as usize).max(1);
            let idx = surfaces.iter().position(|s| s.id == w.id);
            let idx = match idx {
                Some(i) if (surfaces[i].top - w.top).abs() > 0.5 || (surfaces[i].left - w.left).abs() > 0.5 => {
                    // La fenêtre a bougé : sa neige tombe.
                    let old = std::mem::replace(&mut surfaces[i], Surface::new(w.id, w.left, w.top, n));
                    self.drop_bins(old.left, old.top, &old.h, 0..old.h.len());
                    i
                }
                Some(i) => {
                    let s = &mut surfaces[i];
                    if s.h.len() != n {
                        if s.h.len() > n {
                            let tail: Vec<f32> = s.h[n..].to_vec();
                            let x0 = s.left + n as f32 * self.bin;
                            let top = s.top;
                            s.h.truncate(n);
                            s.open.truncate(n);
                            self.drop_bins(x0, top, &tail, 0..tail.len());
                        } else {
                            s.h.resize(n, 0.0);
                            s.open.resize(n, false);
                        }
                    }
                    i
                }
                None => {
                    if surfaces.len() >= MAX_SURFACES {
                        continue;
                    }
                    surfaces.push(Surface::new(w.id, w.left, w.top, n));
                    surfaces.len() - 1
                }
            };
            let s = &mut surfaces[idx];
            s.seen = true;
            // Visibilité de chaque colonne ; ce qui devient caché tombe.
            for i in 0..s.h.len() {
                let x = s.left + (i as f32 + 0.5) * self.bin;
                let open = w.vis.iter().any(|&(a, b)| x >= a && x < b);
                if !open && s.h[i] > 0.0 {
                    let h = [s.h[i]];
                    let x0 = s.left + i as f32 * self.bin;
                    let top = s.top;
                    s.h[i] = 0.0;
                    self.drop_bins(x0, top, &h, 0..1);
                }
                s.open[i] = open;
            }
        }
        // Fenêtres disparues (fermées, minimisées) : la neige tombe d'un coup.
        let mut i = 0;
        while i < surfaces.len() {
            if surfaces[i].seen {
                i += 1;
            } else {
                let s = surfaces.swap_remove(i);
                self.drop_bins(s.left, s.top, &s.h, 0..s.h.len());
            }
        }
        self.surfaces = surfaces;
    }

    pub fn update(&mut self, dt: f32, p: &Params) {
        let dt = dt.clamp(0.0, 0.05);
        if dt <= 0.0 {
            return;
        }
        let s = self.scale;
        self.time += dt;
        let t = self.time;
        // Vent : somme de sinusoïdes lentes, jamais brusque.
        self.wind = s * (13.0 * (t * 0.031).sin() + 8.0 * (t * 0.077 + 1.3).sin() + 4.0 * (t * 0.19 + 0.4).sin());

        // Rafales : une onde lente qui balaie l'écran, modulée dans le temps.
        let gust_env = (0.5 + 0.5 * (t * 0.045 + 2.0).sin()).powi(2);
        let gust_amp = s * 22.0 * gust_env;
        let gust_k = 0.0019 / s;
        let gust_w = 0.55;

        // Apparition (le plan principal garde le débit demandé).
        self.spawn_acc = (self.spawn_acc + p.rate.max(0.0) / MID_SHARE * (self.width / s) / 1000.0 * dt).min(64.0);
        while self.spawn_acc >= 1.0 {
            self.spawn_acc -= 1.0;
            if self.particles.len() < MAX_PARTICLES {
                let f = self.new_flake();
                self.particles.push(f);
            }
        }

        // Curseur.
        let (cvx, cvy) = match (self.cursor, self.prev_cursor) {
            (Some(c), Some(pc)) => (((c.0 - pc.0) / dt).clamp(-6000.0, 6000.0), ((c.1 - pc.1) / dt).clamp(-6000.0, 6000.0)),
            _ => (0.0, 0.0),
        };
        self.prev_cursor = self.cursor;
        let cspeed = (cvx * cvx + cvy * cvy).sqrt();
        let push_r = 34.0 * s;
        let push_r2 = push_r * push_r;

        let decay = (-2.4 * dt).exp();
        let margin = 200.0 * s;
        let vmax = 900.0 * s;
        let mut i = 0;
        while i < self.particles.len() {
            let mut q = self.particles[i];
            let y0 = q.y;
            let cursor = if q.layer == LAYER_FAR { None } else { self.cursor };
            if let Some((cx, cy)) = cursor {
                let dx = q.x - cx;
                let dy = q.y - cy;
                let d2 = dx * dx + dy * dy;
                if d2 < push_r2 && d2 > 1e-4 {
                    let d = d2.sqrt();
                    let f = 1.0 - d / push_r;
                    let push = f * (420.0 * s + 0.5 * cspeed) * 6.0 * dt;
                    q.vx = (q.vx + dx / d * push + cvx * f * 3.0 * dt).clamp(-vmax, vmax);
                    q.vy = (q.vy + dy / d * push + cvy * f * 3.0 * dt).clamp(-vmax, vmax);
                }
            }
            // Flottement : deux harmoniques, plus une légère oscillation verticale.
            let w = q.phase + t * q.freq * TAU;
            let sway = q.amp * q.freq * TAU * (w.cos() + 0.35 * (2.7 * w + 1.7).cos());
            let gust = gust_amp * (q.x * gust_k - t * gust_w + q.y * 0.0007 / s).sin();
            q.x += ((self.wind + gust) * q.drift + sway + q.vx) * dt;
            q.y += (q.fall * (1.0 + 0.18 * (1.3 * w).sin()) + q.vy) * dt;
            q.vx *= decay;
            q.vy *= decay;

            let hit = match q.layer {
                LAYER_MID => self.find_hit(q.x, y0, q.y, p),
                // Plan lointain : disparaît au niveau du sol, sans s'y poser.
                LAYER_FAR => {
                    let no_acc = Params { ground: false, windows: false, ..*p };
                    self.find_hit(q.x, y0, q.y, &no_acc)
                }
                _ => None,
            };
            match hit {
                Some(Hit::Ground(b)) => {
                    self.land_ground(b, &q);
                    self.particles.swap_remove(i);
                    continue;
                }
                Some(Hit::Surface(si, b)) => {
                    self.land_surface(si, b, &q);
                    self.particles.swap_remove(i);
                    continue;
                }
                Some(Hit::Kill) => {
                    self.particles.swap_remove(i);
                    continue;
                }
                None => {}
            }
            if q.y > self.height + 40.0 * s || q.x < -margin * 2.0 || q.x > self.width + margin * 2.0 || q.y < -self.height {
                self.particles.swap_remove(i);
                continue;
            }
            self.particles[i] = q;
            i += 1;
        }

        self.update_piles(dt, p);
        if let Some((cx, cy)) = self.cursor {
            self.carve(cx, cy, cvx, cvy);
        }
    }

    // ---------------------------------------------------------------- interne

    fn new_flake(&mut self) -> Particle {
        let s = self.scale;
        let rng = &mut self.rng;
        let u = rng.f();
        let roll = rng.f();
        let x = rng.range(-200.0 * s, self.width + 200.0 * s);
        let phase = rng.range(0.0, TAU);
        let (layer, rr, alpha, fall, freq, amp, drift);
        if roll < MID_SHARE {
            // Plan principal : surtout des petits flocons, quelques gros.
            layer = LAYER_MID;
            rr = 1.0 + 2.4 * u * u;
            alpha = (0.5 + 0.5 * (rr - 1.0) / 2.4).min(1.0);
            fall = s * (24.0 + 15.0 * rr) * rng.range(0.85, 1.15);
            freq = rng.range(0.12, 0.42);
            amp = s * rng.range(5.0, 20.0);
            drift = 0.45 + 0.55 * rr / 3.4;
        } else if roll < 0.975 {
            // Lointain : minuscule, lent, pâle (effet de profondeur).
            layer = LAYER_FAR;
            rr = 0.6 + 0.6 * u;
            alpha = rng.range(0.22, 0.42);
            fall = s * (13.0 + 8.0 * rr) * rng.range(0.85, 1.15);
            freq = rng.range(0.1, 0.3);
            amp = s * rng.range(2.0, 7.0);
            drift = 0.3;
        } else {
            // Premier plan : gros flocon flou, rapide, très transparent.
            layer = LAYER_NEAR;
            rr = 5.0 + 4.0 * u;
            alpha = rng.range(0.10, 0.20);
            fall = s * rng.range(95.0, 140.0);
            freq = rng.range(0.15, 0.35);
            amp = s * rng.range(18.0, 40.0);
            drift = 1.4;
        }
        let r = s * rr;
        Particle {
            x,
            y: rng.range(-30.0 * s, -4.0 * s) - if layer == LAYER_NEAR { 20.0 * s } else { 0.0 },
            vx: 0.0,
            vy: 0.0,
            r,
            alpha,
            layer,
            fall,
            phase,
            freq,
            amp,
            drift,
            vol: PACKING * PI * r * r,
            lump: false,
        }
    }

    fn debris(&mut self, x: f32, y: f32, vol: f32, vx: f32, vy: f32) -> Particle {
        let s = self.scale;
        let r = (vol / (PACKING * PI)).sqrt().clamp(0.8 * s, 4.5 * s);
        let rr = r / s;
        Particle {
            x,
            y,
            vx,
            vy,
            r,
            alpha: 0.95,
            layer: LAYER_MID,
            fall: s * (55.0 + 22.0 * rr),
            phase: self.rng.range(0.0, TAU),
            freq: self.rng.range(0.1, 0.3),
            amp: s * self.rng.range(1.0, 5.0),
            drift: 0.3,
            vol,
            lump: rr > 2.2,
        }
    }

    /// Transforme des colonnes de tas en morceaux qui tombent.
    fn drop_bins(&mut self, x0: f32, top: f32, h: &[f32], range: std::ops::Range<usize>) {
        let s = self.scale;
        let bin = self.bin;
        let piece_max = PACKING * PI * (4.5 * s) * (4.5 * s);
        let piece_min = PACKING * PI * (0.8 * s) * (0.8 * s);
        let mut i = range.start;
        while i < range.end {
            let j = (i + 3).min(range.end);
            let mut vol: f32 = h[i..j].iter().map(|v| v * bin).sum();
            let hmax = h[i..j].iter().cloned().fold(0.0, f32::max);
            let cx = x0 + (i + j) as f32 * 0.5 * bin;
            while vol > piece_min && self.particles.len() < MAX_PARTICLES {
                let piece = vol.min(piece_max * self.rng.range(0.4, 1.0));
                vol -= piece;
                let x = cx + self.rng.range(-bin, bin);
                let y = top - hmax * self.rng.f();
                let vx = self.rng.range(-30.0, 30.0) * s;
                let vy = self.rng.range(-40.0, 0.0) * s;
                let d = self.debris(x, y, piece, vx, vy);
                self.particles.push(d);
            }
            i = j;
        }
    }

    fn find_hit(&self, x: f32, y0: f32, y1: f32, p: &Params) -> Option<Hit> {
        let bin = self.bin;
        let mut best: Option<(f32, usize, usize)> = None;
        if p.windows {
            for (si, s) in self.surfaces.iter().enumerate() {
                let rel = x - s.left;
                if rel < 0.0 {
                    continue;
                }
                let b = (rel / bin) as usize;
                if b >= s.h.len() || !s.open[b] {
                    continue;
                }
                let ys = s.top - s.h[b];
                if y0 <= ys + 1.0 && y1 >= ys && best.map_or(true, |(by, _, _)| ys < by) {
                    best = Some((ys, si, b));
                }
            }
        }
        if let Some((_, si, b)) = best {
            return Some(Hit::Surface(si, b));
        }
        if x < 0.0 {
            return None;
        }
        let b = (x / bin) as usize;
        let base = *self.ground.base.get(b)?;
        if !base.is_finite() {
            return None;
        }
        let top = if p.ground { base - self.ground.h[b] } else { base };
        if y1 >= top {
            if p.ground && y0 <= base + 2.0 {
                return Some(Hit::Ground(b));
            }
            return Some(Hit::Kill);
        }
        None
    }

    fn land_ground(&mut self, b: usize, q: &Particle) {
        let vol = if q.lump { q.vol * 0.6 } else { q.vol };
        let g = &mut self.ground;
        deposit(&mut g.h, |i| g.link.get(i).copied().unwrap_or(false), b, vol / self.bin, |i| g.cap[i]);
        if q.lump {
            self.burst(q);
        }
    }

    fn land_surface(&mut self, si: usize, b: usize, q: &Particle) {
        let vol = if q.lump { q.vol * 0.6 } else { q.vol };
        let cap = 30.0 * self.scale;
        let s = &mut self.surfaces[si];
        let open = &s.open;
        deposit(&mut s.h, |i| i + 1 < open.len() && open[i] && open[i + 1], b, vol / self.bin, |_| cap);
        if q.lump {
            self.burst(q);
        }
    }

    /// Un gros morceau éclate en poudre à l'impact.
    fn burst(&mut self, q: &Particle) {
        let s = self.scale;
        for _ in 0..3 {
            if self.particles.len() >= MAX_PARTICLES {
                break;
            }
            let vx = self.rng.range(-90.0, 90.0) * s;
            let vy = -self.rng.range(60.0, 170.0) * s;
            let mut d = self.debris(q.x, q.y - 2.0 * s, q.vol * 0.13, vx, vy);
            d.lump = false;
            d.alpha = 0.8;
            self.particles.push(d);
        }
    }

    fn update_piles(&mut self, dt: f32, p: &Params) {
        let s = self.scale;
        let bin = self.bin;
        let melt_k = 1.0 / 420.0;
        let melt_c = 0.012 * s;

        // Sol.
        {
            let g = &mut self.ground;
            let link = &g.link;
            relax(&mut g.h, |i| link[i], bin * 0.8);
            smooth(&mut g.h, |i| link[i], 0.04);
            if p.melt {
                melt(&mut g.h, dt, melt_k, melt_c);
            }
        }

        // Fenêtres.
        let unit = PACKING * PI * (1.8 * s) * (1.8 * s);
        let edge = bin * 1.5;
        let mut spills: Vec<(f32, f32, f32)> = Vec::new();
        for sf in &mut self.surfaces {
            let n = sf.h.len();
            if n == 0 {
                continue;
            }
            {
                let open = &sf.open;
                relax(&mut sf.h, |i| open[i] && open[i + 1], bin);
                smooth(&mut sf.h, |i| open[i] && open[i + 1], 0.04);
            }
            // Chaque bord (bout de fenêtre ou portion masquée) déborde :
            // la neige glisse et tombe au lieu de former des pics.
            for i in 0..n {
                if !sf.open[i] || sf.h[i] <= edge {
                    continue;
                }
                let left_edge = i == 0 || !sf.open[i - 1];
                let right_edge = i + 1 == n || !sf.open[i + 1];
                if !left_edge && !right_edge {
                    continue;
                }
                let t = (sf.h[i] - edge) * 0.3;
                sf.h[i] -= t;
                if left_edge {
                    sf.spill_l += t * bin;
                    if sf.spill_l >= unit {
                        sf.spill_l -= unit;
                        spills.push((sf.left + i as f32 * bin - 1.5 * s, sf.top - 2.0 * s, -1.0));
                    }
                } else {
                    sf.spill_r += t * bin;
                    if sf.spill_r >= unit {
                        sf.spill_r -= unit;
                        spills.push((sf.left + (i + 1) as f32 * bin + 1.5 * s, sf.top - 2.0 * s, 1.0));
                    }
                }
            }
            if p.melt {
                melt(&mut sf.h, dt, melt_k, melt_c);
            }
        }
        for (x, y, dir) in spills {
            if self.particles.len() < MAX_PARTICLES {
                let vx = dir * self.rng.range(10.0, 45.0) * s;
                let d = self.debris(x, y, unit, vx, 0.0);
                self.particles.push(d);
            }
        }
    }

    /// Le curseur creuse les tas et projette de la poudre.
    fn carve(&mut self, cx: f32, cy: f32, cvx: f32, cvy: f32) {
        let s = self.scale;
        let r = 22.0 * s;
        let bin = self.bin;
        let mut removed = 0.0;
        {
            let g = &mut self.ground;
            let base = &g.base;
            removed += carve_pile(&mut g.h, 0.0, bin, cx, cy, r, |i| base[i].is_finite(), |i| base[i]);
        }
        for sf in &mut self.surfaces {
            let open = &sf.open;
            let top = sf.top;
            removed += carve_pile(&mut sf.h, sf.left, bin, cx, cy, r, |i| open[i], |_| top);
        }
        if removed <= 0.0 {
            return;
        }
        self.spray_acc += removed;
        let unit = PACKING * PI * (1.4 * s) * (1.4 * s);
        let mut n = 0;
        while self.spray_acc >= unit && n < 30 && self.particles.len() < MAX_PARTICLES {
            self.spray_acc -= unit;
            n += 1;
            let x = cx + self.rng.range(-r, r);
            let y = cy - self.rng.range(0.0, r);
            let dir = if x < cx { -1.0 } else { 1.0 };
            let vx = dir * self.rng.range(40.0, 170.0) * s + cvx * 0.25;
            let vy = -self.rng.range(80.0, 260.0) * s + cvy * 0.2;
            let mut d = self.debris(x, y, unit, vx, vy);
            d.alpha = 0.85;
            self.particles.push(d);
        }
        if n >= 30 {
            self.spray_acc = 0.0;
        }
    }
}

impl Surface {
    fn new(id: isize, left: f32, top: f32, n: usize) -> Self {
        Surface { id, left, top, h: vec![0.0; n], open: vec![false; n], spill_l: 0.0, spill_r: 0.0, seen: true }
    }
}

/// Dépose `dh` (hauteur équivalente sur une colonne) autour de `b`.
fn deposit(h: &mut [f32], link: impl Fn(usize) -> bool, b: usize, dh: f32, cap: impl Fn(usize) -> f32) {
    let n = h.len();
    if b >= n {
        return;
    }
    // Noyau en cloche [1 4 6 4 1]/16 : dépôt doux, pas de grain.
    // Ce qui tomberait au-delà d'un mur revient à la colonne centrale.
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let l1 = b > 0 && link(b - 1);
    let l2 = l1 && b > 1 && link(b - 2);
    let r1 = b + 1 < n && link(b);
    let r2 = r1 && b + 2 < n && link(b + 1);
    let mut center = K[2];
    let mut add = |i: usize, w: f32, ok: bool, center: &mut f32| {
        if ok {
            h[i] = (h[i] + dh * w).min(cap(i));
        } else {
            *center += w;
        }
    };
    add(b.wrapping_sub(2), K[0], l2, &mut center);
    add(b.wrapping_sub(1), K[1], l1, &mut center);
    add(b + 1, K[3], r1, &mut center);
    add(b + 2, K[4], r2, &mut center);
    h[b] = (h[b] + dh * center).min(cap(b));
}

/// Diffusion légère (conserve la masse) : gomme le bruit de surface sans
/// aplatir les grandes formes.
fn smooth(h: &mut [f32], link: impl Fn(usize) -> bool, k: f32) {
    let n = h.len();
    if n < 2 {
        return;
    }
    let mut prev_flux = 0.0;
    for i in 0..n - 1 {
        let flux = if link(i) { k * (h[i] - h[i + 1]) } else { 0.0 };
        h[i] = (h[i] - flux + prev_flux).max(0.0);
        prev_flux = flux;
    }
    h[n - 1] = (h[n - 1] + prev_flux).max(0.0);
}

/// Éboulement : limite la pente entre colonnes voisines (conserve la masse).
fn relax(h: &mut [f32], link: impl Fn(usize) -> bool, maxd: f32) {
    let n = h.len();
    if n < 2 {
        return;
    }
    let mut step = |i: usize| {
        if !link(i) {
            return;
        }
        let d = h[i] - h[i + 1];
        if d > maxd {
            let t = (d - maxd) * 0.3;
            h[i] -= t;
            h[i + 1] += t;
        } else if -d > maxd {
            let t = (-d - maxd) * 0.3;
            h[i] += t;
            h[i + 1] -= t;
        }
    };
    for i in 0..n - 1 {
        step(i);
    }
    for i in (0..n - 1).rev() {
        step(i);
    }
}

fn melt(h: &mut [f32], dt: f32, k: f32, c: f32) {
    for v in h.iter_mut() {
        if *v > 0.0 {
            *v = (*v - (*v * k + c) * dt).max(0.0);
        }
    }
}

fn carve_pile(
    h: &mut [f32],
    x0: f32,
    bin: f32,
    cx: f32,
    cy: f32,
    r: f32,
    open: impl Fn(usize) -> bool,
    base: impl Fn(usize) -> f32,
) -> f32 {
    let n = h.len() as isize;
    let a = (((cx - r - x0) / bin).floor() as isize).max(0);
    let b = (((cx + r - x0) / bin).ceil() as isize).min(n - 1);
    let mut removed = 0.0;
    let mut i = a;
    while i <= b {
        let k = i as usize;
        i += 1;
        if h[k] <= 0.0 || !open(k) {
            continue;
        }
        let dx = x0 + (k as f32 + 0.5) * bin - cx;
        if dx.abs() >= r {
            continue;
        }
        let half = (r * r - dx * dx).sqrt();
        let base = base(k);
        let top = base - h[k];
        if cy - half < base && cy + half > top {
            let nh = (base - (cy + half)).max(0.0);
            if nh < h[k] {
                removed += (h[k] - nh) * bin;
                h[k] = nh;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_screen() -> Vec<Monitor> {
        vec![Monitor {
            rect: Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1080.0 },
            work: Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1032.0 },
        }]
    }

    #[test]
    fn rng_in_range() {
        let mut r = Rng::new(42);
        for _ in 0..10_000 {
            let v = r.f();
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn relax_conserves_mass() {
        let mut h = vec![0.0, 0.0, 50.0, 0.0, 0.0];
        let before: f32 = h.iter().sum();
        for _ in 0..200 {
            relax(&mut h, |_| true, 1.0);
        }
        let after: f32 = h.iter().sum();
        assert!((before - after).abs() < 1e-3);
        for w in h.windows(2) {
            assert!((w[0] - w[1]).abs() <= 1.0 + 1e-3);
        }
    }

    #[test]
    fn snow_accumulates_on_ground_and_stays_bounded() {
        let mut sim = Sim::new(1920.0, 1080.0, 1.0, &one_screen(), false, 1);
        let p = Params { rate: 28.0, ground: true, windows: true, melt: false };
        for _ in 0..(60 * 120) {
            sim.update(1.0 / 60.0, &p);
        }
        assert!(sim.total_snow() > 0.0);
        assert!(sim.particles.len() <= MAX_PARTICLES);
        let cap = 1080.0 * 0.16;
        assert!(sim.ground.h.iter().all(|h| h.is_finite() && *h >= 0.0 && *h <= cap + 1e-3));
    }

    #[test]
    fn closed_window_drops_its_snow() {
        let mut sim = Sim::new(1920.0, 1080.0, 1.0, &one_screen(), false, 2);
        let w = WinTop { id: 7, left: 100.0, right: 900.0, top: 400.0, vis: vec![(100.0, 900.0)] };
        sim.sync_windows(std::slice::from_ref(&w));
        let p = Params { rate: 28.0, ground: false, windows: true, melt: false };
        for _ in 0..(60 * 60) {
            sim.update(1.0 / 60.0, &p);
        }
        let on_win: f32 = sim.surfaces[0].h.iter().sum();
        assert!(on_win > 0.0, "la neige doit se poser sur la fenêtre");
        let before = sim.particles.len();
        sim.sync_windows(&[]);
        assert!(sim.surfaces.is_empty());
        assert!(sim.particles.len() > before, "la neige doit tomber");
    }

    #[test]
    fn no_spikes_in_narrow_gaps() {
        let mut sim = Sim::new(1920.0, 1080.0, 1.0, &one_screen(), false, 5);
        // Bord presque entièrement masqué : seule une fente de 6 px est visible.
        let w = WinTop { id: 9, left: 100.0, right: 900.0, top: 400.0, vis: vec![(500.0, 506.0)] };
        let p = Params { rate: 28.0, ground: false, windows: true, melt: false };
        for _ in 0..(60 * 120) {
            sim.sync_windows(std::slice::from_ref(&w));
            sim.update(1.0 / 60.0, &p);
        }
        let max = sim.surfaces[0].h.iter().cloned().fold(0.0, f32::max);
        // Sans la correction : ~30 px (plafond). Avec : quelques px au plus.
        assert!(max <= sim.bin * 4.0, "pic de {max} px dans une fente");
    }

    #[test]
    fn deposit_and_smooth_conserve_mass() {
        let mut h = vec![0.0f32; 20];
        deposit(&mut h, |i| i != 9, 10, 16.0, |_| 1000.0);
        deposit(&mut h, |_| true, 0, 16.0, |_| 1000.0);
        let sum: f32 = h.iter().sum();
        assert!((sum - 32.0).abs() < 1e-3);
        for _ in 0..500 {
            smooth(&mut h, |i| i != 9, 0.04);
        }
        let after: f32 = h.iter().sum();
        assert!((after - 32.0).abs() < 1e-2);
        // Rien ne traverse le mur entre 9 et 10.
        let left: f32 = h[..10].iter().sum();
        assert!((left - 16.0).abs() < 1e-2);
    }

    #[test]
    fn cursor_carves_pile() {
        let mut sim = Sim::new(800.0, 600.0, 1.0, &[Monitor {
            rect: Rect { l: 0.0, t: 0.0, r: 800.0, b: 600.0 },
            work: Rect { l: 0.0, t: 0.0, r: 800.0, b: 600.0 },
        }], false, 3);
        sim.ground.h.iter_mut().for_each(|h| *h = 40.0);
        sim.set_cursor(Some((400.0, 580.0)));
        let p = Params { rate: 0.0, ground: true, windows: false, melt: false };
        sim.update(1.0 / 60.0, &p);
        let b = (400.0 / sim.bin) as usize;
        assert!(sim.ground.h[b] < 40.0);
        assert!(!sim.particles.is_empty());
    }

    #[test]
    fn geometry_change_is_safe() {
        let mut sim = Sim::new(1920.0, 1080.0, 1.5, &one_screen(), false, 4);
        let p = Params::default();
        for _ in 0..600 {
            sim.update(1.0 / 60.0, &p);
        }
        sim.set_geometry(1280.0, 720.0, &[], true);
        for _ in 0..600 {
            sim.update(1.0 / 60.0, &p);
        }
        assert!(sim.ground.h.iter().all(|h| *h == 0.0));
    }
}
