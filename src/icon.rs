//! Icône flocon générée à la volée (pas de ressource à embarquer).

use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let (qx, qy) = (ax + t * dx - px, ay + t * dy - py);
    (qx * qx + qy * qy).sqrt()
}

/// Pixels BGRA prémultipliés, `size` x `size`.
pub fn snowflake_pixels(size: u32) -> Vec<u8> {
    let n = size as f32;
    let c = n / 2.0;
    let arm = n * 0.44;
    let width = (n * 0.055).max(0.75);
    let mut segs = Vec::new();
    for k in 0..6 {
        let a = std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 3.0;
        let (s, co) = a.sin_cos();
        let (ex, ey) = (c + co * arm, c - s * arm);
        segs.push((c, c, ex, ey));
        let (mx, my) = (c + co * arm * 0.58, c - s * arm * 0.58);
        for side in [-1.0f32, 1.0] {
            let b = a + side * 0.75;
            let (bs, bc) = b.sin_cos();
            segs.push((mx, my, mx + bc * arm * 0.33, my - bs * arm * 0.33));
        }
    }
    // Bleu glacier : lisible sur barre des tâches claire comme sombre.
    let (r, g, b) = (0x3Bu8 as f32, 0xA7u8 as f32, 0xF0u8 as f32);
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = segs.iter().map(|&(ax, ay, bx, by)| seg_dist(px, py, ax, ay, bx, by)).fold(f32::MAX, f32::min);
            let a = (width + 0.5 - d).clamp(0.0, 1.0);
            let i = ((y * size + x) * 4) as usize;
            out[i] = (b * a) as u8;
            out[i + 1] = (g * a) as u8;
            out[i + 2] = (r * a) as u8;
            out[i + 3] = (255.0 * a) as u8;
        }
    }
    out
}

/// Écrit un fichier .ico multi-tailles (BMP 32 bits avec alpha).
pub fn write_ico(path: &std::path::Path) -> std::io::Result<()> {
    const SIZES: [u32; 6] = [16, 20, 24, 32, 48, 64];
    let mut images = Vec::new();
    for &s in &SIZES {
        let px = snowflake_pixels(s);
        let mut img = Vec::new();
        // BITMAPINFOHEADER (hauteur doublée : couleur + masque).
        img.extend_from_slice(&40u32.to_le_bytes());
        img.extend_from_slice(&(s as i32).to_le_bytes());
        img.extend_from_slice(&((s * 2) as i32).to_le_bytes());
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        img.extend_from_slice(&[0u8; 24]);
        // Lignes de bas en haut.
        for y in (0..s).rev() {
            let row = (y * s * 4) as usize;
            img.extend_from_slice(&px[row..row + (s * 4) as usize]);
        }
        // Masque ET : tout à zéro (l'alpha fait foi), lignes alignées sur 32 bits.
        let mask_row = s.div_ceil(32) * 4;
        img.extend(std::iter::repeat_n(0u8, (mask_row * s) as usize));
        images.push((s, img));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, img) in &images {
        out.push(*s as u8);
        out.push(*s as u8);
        out.extend_from_slice(&[0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for (_, img) in &images {
        out.extend_from_slice(img);
    }
    std::fs::write(path, out)
}

pub fn create_icon(size: u32) -> Option<HICON> {
    let size = size.clamp(16, 256);
    let px = snowflake_pixels(size);
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(color.into());
            return None;
        }
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
        let mask_bits = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
