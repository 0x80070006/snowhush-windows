//! Suivi des fenêtres : bords supérieurs visibles, dans l'ordre z.
//! Un scan = un EnumWindows (quelques dizaines de µs), sans allocation
//! en régime établi (tampons réutilisés).

use crate::sim::WinTop;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy)]
struct Win {
    id: isize,
    rect: RECT,
    candidate: bool,
}

pub struct Tracker {
    wins: Vec<Win>,
    out: Vec<WinTop>,
    pid: u32,
}

struct Ctx<'a> {
    wins: &'a mut Vec<Win>,
    pid: u32,
}

impl Tracker {
    pub fn new() -> Self {
        Tracker { wins: Vec::with_capacity(256), out: Vec::with_capacity(64), pid: std::process::id() }
    }

    /// `origin` = coin de l'écran virtuel ; `monitors` en coordonnées écran.
    pub fn scan(&mut self, origin: (i32, i32), monitors: &[RECT], scale: f32) -> &[WinTop] {
        self.wins.clear();
        let mut ctx = Ctx { wins: &mut self.wins, pid: self.pid };
        unsafe {
            let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut Ctx as isize));
        }

        let mut used = 0;
        let margin = (8.0 * scale) as i32;
        for (i, w) in self.wins.iter().enumerate() {
            if !w.candidate || used >= crate::sim::MAX_SURFACES {
                continue;
            }
            let r = w.rect;
            let top = r.top;
            // Le bord doit être sur un écran, avec de la place au-dessus.
            let mid = (r.left.max(i32::MIN / 2) + r.right) / 2;
            let on_screen = monitors.iter().any(|m| {
                let x = mid.clamp(m.left, m.right - 1);
                x >= m.left && x < m.right && r.right > m.left && r.left < m.right && top >= m.top + margin && top < m.bottom - margin
            });
            if !on_screen {
                continue;
            }
            if used >= self.out.len() {
                self.out.push(WinTop::default());
            }
            let t = &mut self.out[used];
            t.vis.clear();
            t.vis.push((r.left as f32, r.right as f32));
            // Retire les portions recouvertes par les fenêtres au-dessus.
            for o in &self.wins[..i] {
                let or = o.rect;
                if or.top <= top + 2 && or.bottom >= top - 4 && or.right > r.left && or.left < r.right {
                    subtract(&mut t.vis, or.left as f32, or.right as f32);
                    if t.vis.is_empty() {
                        break;
                    }
                }
            }
            if t.vis.is_empty() {
                continue;
            }
            let (ox, oy) = (origin.0 as f32, origin.1 as f32);
            for v in t.vis.iter_mut() {
                v.0 -= ox;
                v.1 -= ox;
            }
            t.id = w.id;
            t.left = r.left as f32 - ox;
            t.right = r.right as f32 - ox;
            t.top = top as f32 - oy;
            used += 1;
        }
        &self.out[..used]
    }
}

fn subtract(v: &mut Vec<(f32, f32)>, a: f32, b: f32) {
    let mut i = 0;
    while i < v.len() {
        let (l, r) = v[i];
        if b <= l || a >= r {
            i += 1;
            continue;
        }
        match (a > l, b < r) {
            (true, true) => {
                v[i] = (l, a);
                v.insert(i + 1, (b, r));
                i += 2;
            }
            (true, false) => {
                v[i].1 = a;
                i += 1;
            }
            (false, true) => {
                v[i].0 = b;
                i += 1;
            }
            (false, false) => {
                v.remove(i);
            }
        }
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    let ctx = &mut *(lp.0 as *mut Ctx);
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return true.into();
    }
    let mut cloaked: u32 = 0;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok() && cloaked != 0 {
        return true.into();
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TRANSPARENT.0 != 0 {
        return true.into(); // overlays transparents aux clics (dont le nôtre)
    }
    if ex & WS_EX_LAYERED.0 != 0 {
        // Fenêtre "visible" mais totalement transparente : ne masque rien.
        let mut alpha = 255u8;
        let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
        if GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok()
            && flags.0 & LWA_ALPHA.0 != 0
            && alpha == 0
        {
            return true.into();
        }
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == ctx.pid {
        return true.into();
    }
    let mut rect = RECT::default();
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rect as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_err()
        && GetWindowRect(hwnd, &mut rect).is_err()
    {
        return true.into();
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w < 4 || h < 4 {
        return true.into();
    }
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    let class = &buf[..n.min(buf.len())];
    if eq(class, "Progman") || eq(class, "WorkerW") {
        return true.into(); // bureau
    }
    let shell = eq(class, "Shell_TrayWnd") || eq(class, "Shell_SecondaryTrayWnd");
    let tool = ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0;
    let candidate = !shell && !tool && w >= 120 && h >= 60;
    ctx.wins.push(Win { id: hwnd.0 as isize, rect, candidate });
    true.into()
}

fn eq(w: &[u16], s: &str) -> bool {
    w.len() == s.len() && w.iter().zip(s.bytes()).all(|(a, b)| *a == b as u16)
}

#[cfg(test)]
mod tests {
    use super::subtract;

    #[test]
    fn interval_subtraction() {
        let mut v = vec![(0.0, 100.0)];
        subtract(&mut v, 20.0, 30.0);
        assert_eq!(v, vec![(0.0, 20.0), (30.0, 100.0)]);
        subtract(&mut v, -5.0, 10.0);
        assert_eq!(v, vec![(10.0, 20.0), (30.0, 100.0)]);
        subtract(&mut v, 15.0, 200.0);
        assert_eq!(v, vec![(10.0, 15.0)]);
        subtract(&mut v, 0.0, 200.0);
        assert!(v.is_empty());
    }
}
