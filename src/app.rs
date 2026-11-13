//! Application : fenêtres, icône de notification, menu, cycle de vie.

use crate::render::Renderer;
use crate::settings::{self, Fps, Settings, INTENSITY_NAMES, INTENSITY_RATES, OPACITIES};
use crate::sim::{Monitor, Params, Rect, Sim};
use crate::tracker::Tracker;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRAY: u32 = WM_APP + 1;
const TIMER_STATE: usize = 1;
const TIMER_REBUILD: usize = 2;
const TIMER_RETRY: usize = 3;

const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 7;
const WTS_SESSION_UNLOCK: usize = 8;
const PBT_APMSUSPEND: usize = 0x4;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
const PBT_POWERSETTINGCHANGE: usize = 0x8013;

// Identifiants de menu.
const ID_INTENSITY: usize = 100;
const ID_OPACITY: usize = 110;
const ID_FPS30: usize = 120;
const ID_FPS60: usize = 121;
const ID_FPSMAX: usize = 122;
const ID_ACC_GROUND: usize = 130;
const ID_ACC_WINDOWS: usize = 131;
const ID_MELT: usize = 140;
const ID_FULLSCREEN: usize = 141;
const ID_CAPTURE: usize = 142;
const ID_TASKBAR: usize = 143;
const ID_AUTOSTART: usize = 144;
const ID_PAUSE: usize = 150;
const ID_CLEAR: usize = 151;
const ID_QUIT: usize = 199;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Accès à l'état global. Renvoie None si déjà emprunté (ré-entrance) :
/// le message est alors simplement ignoré, jamais de panique.
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn install(app: App) {
    APP.with(|c| *c.borrow_mut() = Some(app));
}

pub fn uninstall() {
    let app = APP.with(|c| c.borrow_mut().take());
    drop(app);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Geo {
    origin: (i32, i32),
    w: i32,
    h: i32,
    screens: Vec<RECT>,
    monitors: Vec<Monitor>,
    scale: f32,
    refresh: f32,
}

unsafe extern "system" fn monitor_cb(hm: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<(RECT, RECT)>);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(hm, &mut mi).as_bool() {
        v.push((mi.rcMonitor, mi.rcWork));
    }
    true.into()
}

fn query_geometry() -> Geo {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        let mut list: Vec<(RECT, RECT)> = Vec::new();
        let _ = EnumDisplayMonitors(None, None, Some(monitor_cb), LPARAM(&mut list as *mut _ as isize));
        if list.is_empty() {
            let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
            list.push((r, r));
        }
        let conv = |r: RECT| Rect {
            l: (r.left - x) as f32,
            t: (r.top - y) as f32,
            r: (r.right - x) as f32,
            b: (r.bottom - y) as f32,
        };
        let monitors = list.iter().map(|(m, wk)| Monitor { rect: conv(*m), work: conv(*wk) }).collect();
        let screens = list.iter().map(|(m, _)| *m).collect();
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        let refresh = if EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() && dm.dmDisplayFrequency > 1 {
            dm.dmDisplayFrequency as f32
        } else {
            60.0
        };
        Geo { origin: (x, y), w, h, screens, monitors, scale: GetDpiForSystem() as f32 / 96.0, refresh }
    }
}

pub struct App {
    ctl: HWND,
    overlay: HWND,
    settings: Settings,
    sim: Sim,
    renderer: Option<Renderer>,
    tracker: Tracker,
    geo: Geo,
    last: Instant,
    dt_smooth: f32,
    frame_no: u64,
    locked: bool,
    display_off: bool,
    suspended: bool,
    fullscreen: bool,
    on_battery: bool,
    shown: bool,
    icon: Option<HICON>,
    render_failures: u32,
}

impl App {
    pub fn new(ctl: HWND, overlay: HWND) -> App {
        let settings = Settings::load();
        let geo = query_geometry();
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) ^ (std::process::id() as u64) << 32;
        let sim = Sim::new(geo.w as f32, geo.h as f32, geo.scale, &geo.monitors, settings.behind_taskbar, seed);
        let icon = crate::icon::create_icon(unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) } as u32);
        let mut app = App {
            ctl,
            overlay,
            settings,
            sim,
            renderer: None,
            tracker: Tracker::new(),
            geo,
            last: Instant::now(),
            dt_smooth: 1.0 / 60.0,
            frame_no: 0,
            locked: false,
            display_off: false,
            suspended: false,
            fullscreen: false,
            on_battery: false,
            shown: false,
            icon,
            render_failures: 0,
        };
        app.place_overlay();
        app.apply_capture();
        app.ensure_renderer();
        app.add_tray();
        app.poll_state();
        app.update_visibility();
        unsafe {
            SetTimer(Some(ctl), TIMER_STATE, 1000, None);
        }
        settings::refresh_autostart_path();
        app
    }

    // ------------------------------------------------------------ boucle

    /// Vrai si une image doit être produite (sinon : zéro CPU, attente pure).
    pub fn running(&self) -> bool {
        self.shown && self.renderer.is_some()
    }

    pub fn waitable(&self) -> Option<HANDLE> {
        self.renderer.as_ref().map(|r| r.waitable())
    }

    fn params(&self) -> Params {
        Params {
            rate: INTENSITY_RATES[self.settings.intensity.min(INTENSITY_RATES.len() - 1)],
            ground: self.settings.acc_ground,
            windows: self.settings.acc_windows,
            melt: self.settings.melt,
        }
    }

    fn sync_interval(&self) -> u32 {
        let refresh = self.geo.refresh;
        let target = match self.settings.fps {
            Fps::Eco30 => 30.0,
            Fps::Smooth60 if self.on_battery => 30.0,
            Fps::Smooth60 => 60.0,
            Fps::Max if self.on_battery => 60.0,
            Fps::Max => refresh,
        };
        ((refresh / target).round() as u32).clamp(1, 4)
    }

    pub fn frame(&mut self) {
        let now = Instant::now();
        let raw = (now - self.last).as_secs_f32().min(0.05);
        self.last = now;
        // Pas de temps calé sur la cadence d'affichage quand elle est
        // régulière : supprime la micro-saccade due à la gigue du minuteur.
        let expected = self.sync_interval() as f32 / self.geo.refresh.max(1.0);
        let dt = if (raw - expected).abs() < expected * 0.3 {
            self.dt_smooth += (expected - self.dt_smooth) * 0.2;
            self.dt_smooth
        } else {
            self.dt_smooth = expected;
            raw
        };
        self.frame_no += 1;

        if self.settings.acc_windows && self.frame_no % 3 == 0 {
            let wins = self.tracker.scan(self.geo.origin, &self.geo.screens, self.geo.scale);
            self.sim.sync_windows(wins);
        }
        let mut pt = POINT::default();
        let cursor = if unsafe { GetCursorPos(&mut pt) }.is_ok() {
            let (x, y) = ((pt.x - self.geo.origin.0) as f32, (pt.y - self.geo.origin.1) as f32);
            (x >= 0.0 && y >= 0.0 && x < self.geo.w as f32 && y < self.geo.h as f32).then_some((x, y))
        } else {
            None
        };
        self.sim.set_cursor(cursor);
        let params = self.params();
        self.sim.update(dt, &params);

        let opacity = self.settings.opacity as f32 / 100.0;
        let interval = self.sync_interval();
        if let Some(r) = self.renderer.as_mut() {
            if let Err(e) = r.render(&self.sim, opacity, interval) {
                crate::log::write(&format!("rendu : {e:?} — recréation du périphérique"));
                self.renderer = None;
                self.render_failures += 1;
                self.ensure_renderer();
            } else {
                self.render_failures = 0;
            }
        }
    }

    fn ensure_renderer(&mut self) {
        if self.renderer.is_some() {
            return;
        }
        match Renderer::new(self.overlay, self.geo.w as u32, self.geo.h as u32) {
            Ok(r) => {
                self.renderer = Some(r);
                unsafe {
                    let _ = KillTimer(Some(self.ctl), TIMER_RETRY);
                }
            }
            Err(e) => {
                crate::log::write(&format!("création du rendu impossible : {e:?}"));
                // Nouvel essai plus tard (pilote en cours de mise à jour, etc.).
                let delay = (2000 * (1 + self.render_failures.min(14))).min(30_000);
                unsafe {
                    SetTimer(Some(self.ctl), TIMER_RETRY, delay, None);
                }
            }
        }
    }

    // ------------------------------------------------------------ état

    fn place_overlay(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.overlay,
                Some(HWND_TOPMOST),
                self.geo.origin.0,
                self.geo.origin.1,
                self.geo.w,
                self.geo.h,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    fn apply_capture(&self) {
        let aff = if self.settings.in_capture { WDA_NONE } else { WDA_EXCLUDEFROMCAPTURE };
        unsafe {
            if SetWindowDisplayAffinity(self.overlay, aff).is_err() && aff == WDA_EXCLUDEFROMCAPTURE {
                // Windows < 10 2004 : WDA_MONITOR à défaut.
                let _ = SetWindowDisplayAffinity(self.overlay, WDA_MONITOR);
            }
        }
    }

    fn poll_state(&mut self) {
        unsafe {
            // QUNS_BUSY n'est pas utilisé : notre propre overlay (plein écran,
            // topmost) le déclenche, ce qui ferait clignoter la neige.
            let st = SHQueryUserNotificationState().unwrap_or(QUNS_ACCEPTS_NOTIFICATIONS);
            self.fullscreen =
                st == QUNS_RUNNING_D3D_FULL_SCREEN || st == QUNS_PRESENTATION_MODE || self.foreground_is_fullscreen();
            let mut ps = SYSTEM_POWER_STATUS::default();
            self.on_battery = GetSystemPowerStatus(&mut ps).is_ok() && (ps.ACLineStatus == 0 || ps.SystemStatusFlag == 1);
        }
    }

    /// La fenêtre au premier plan (hors bureau, barre des tâches et nous)
    /// couvre-t-elle entièrement son écran ?
    fn foreground_is_fullscreen(&self) -> bool {
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_invalid() || fg == self.overlay || fg == self.ctl || fg == GetShellWindow() || fg == GetDesktopWindow() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(fg, Some(&mut pid));
            if pid == std::process::id() {
                return false;
            }
            let mut buf = [0u16; 32];
            let n = GetClassNameW(fg, &mut buf).max(0) as usize;
            let class = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
            if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
                return false;
            }
            let mut r = RECT::default();
            if GetWindowRect(fg, &mut r).is_err() {
                return false;
            }
            let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
            if mon.is_invalid() {
                return false;
            }
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(mon, &mut mi).as_bool() {
                return false;
            }
            let m = mi.rcMonitor;
            r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
        }
    }

    fn update_visibility(&mut self) {
        let s = &self.settings;
        let show = !(s.paused || self.locked || self.display_off || self.suspended || (self.fullscreen && !s.over_fullscreen));
        if show == self.shown {
            return;
        }
        self.shown = show;
        crate::log::write(&format!(
            "visible={show} (pause={} verrou={} ecran_eteint={} veille={} plein_ecran={})",
            s.paused, self.locked, self.display_off, self.suspended, self.fullscreen
        ));
        unsafe {
            if show {
                let _ = ShowWindow(self.overlay, SW_SHOWNOACTIVATE);
                self.place_overlay();
                self.last = Instant::now();
            } else {
                let _ = ShowWindow(self.overlay, SW_HIDE);
            }
        }
    }

    fn rebuild_geometry(&mut self) {
        let geo = query_geometry();
        let size_changed = geo.w != self.geo.w || geo.h != self.geo.h;
        self.geo = geo;
        self.sim.set_geometry(self.geo.w as f32, self.geo.h as f32, &self.geo.monitors, self.settings.behind_taskbar);
        self.place_overlay();
        if size_changed {
            if let Some(r) = self.renderer.as_mut() {
                if let Err(e) = r.resize(self.geo.w as u32, self.geo.h as u32) {
                    crate::log::write(&format!("redimensionnement : {e:?}"));
                    self.renderer = None;
                }
            }
        }
        self.ensure_renderer();
    }

    // ------------------------------------------------------------ messages

    pub fn on_timer(&mut self, id: usize) {
        unsafe {
            match id {
                TIMER_STATE => {
                    self.poll_state();
                    self.update_visibility();
                    if self.shown {
                        // D'autres fenêtres "topmost" peuvent passer devant.
                        let _ = SetWindowPos(self.overlay, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    }
                }
                TIMER_REBUILD => {
                    let _ = KillTimer(Some(self.ctl), TIMER_REBUILD);
                    self.rebuild_geometry();
                }
                TIMER_RETRY => {
                    let _ = KillTimer(Some(self.ctl), TIMER_RETRY);
                    self.ensure_renderer();
                }
                _ => {}
            }
        }
    }

    pub fn schedule_rebuild(&self) {
        unsafe {
            SetTimer(Some(self.ctl), TIMER_REBUILD, 600, None);
        }
    }

    pub fn set_locked(&mut self, v: bool) {
        self.locked = v;
        self.update_visibility();
    }

    pub fn set_display_off(&mut self, v: bool) {
        self.display_off = v;
        self.update_visibility();
    }

    pub fn set_suspended(&mut self, v: bool) {
        self.suspended = v;
        if !v {
            // Le pilote a pu être réinitialisé pendant la veille.
            self.renderer = None;
            self.schedule_rebuild();
        }
        self.update_visibility();
    }

    // ------------------------------------------------------------ tray

    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.ctl,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_TIP | NIF_ICON,
            uCallbackMessage: WM_TRAY,
            ..Default::default()
        };
        if let Some(i) = self.icon {
            nid.hIcon = i;
        }
        let tip = if self.settings.paused { "Snowhush — en pause" } else { "Snowhush — il neige" };
        for (d, s) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *d = s;
        }
        nid
    }

    pub fn add_tray(&mut self) {
        let nid = self.tray_data();
        unsafe {
            if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            }
        }
    }

    fn update_tray(&self) {
        let nid = self.tray_data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn remove_tray(&self) {
        let nid = self.tray_data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    pub fn build_menu(&self) -> Option<HMENU> {
        let s = &self.settings;
        unsafe {
            let root = CreatePopupMenu().ok()?;
            let add = |m: HMENU, id: usize, text: &str, checked: bool, enabled: bool| {
                let t = wide(text);
                let mut f = MF_STRING;
                if checked {
                    f |= MF_CHECKED;
                }
                if !enabled {
                    f |= MF_GRAYED;
                }
                let _ = AppendMenuW(m, f, id, PCWSTR(t.as_ptr()));
            };
            let sub = |m: HMENU, child: HMENU, text: &str| {
                let t = wide(text);
                let _ = AppendMenuW(m, MF_POPUP, child.0 as usize, PCWSTR(t.as_ptr()));
            };
            let sep = |m: HMENU| {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            };

            add(root, ID_PAUSE, if s.paused { "Reprendre la neige" } else { "Mettre en pause" }, false, true);
            sep(root);

            if let Ok(m) = CreatePopupMenu() {
                for (i, name) in INTENSITY_NAMES.iter().enumerate() {
                    add(m, ID_INTENSITY + i, name, s.intensity == i, true);
                }
                sub(root, m, &format!("Intensité : {}", INTENSITY_NAMES[s.intensity]));
            }
            if let Ok(m) = CreatePopupMenu() {
                for (i, o) in OPACITIES.iter().enumerate() {
                    add(m, ID_OPACITY + i, &format!("{o} %"), s.opacity == *o, true);
                }
                sub(root, m, &format!("Opacité : {} %", s.opacity));
            }
            if let Ok(m) = CreatePopupMenu() {
                add(m, ID_ACC_GROUND, "Sol", s.acc_ground, true);
                add(m, ID_ACC_WINDOWS, "Fenêtres", s.acc_windows, true);
                sub(root, m, "Accumuler sur");
            }
            add(root, ID_CLEAR, "Déblayer la neige", false, true);
            sep(root);

            if let Ok(m) = CreatePopupMenu() {
                add(m, ID_MELT, "Fonte de la neige", s.melt, true);
                add(m, ID_TASKBAR, "Neige sous la barre des tâches", s.behind_taskbar, true);
                add(m, ID_FULLSCREEN, "Afficher par-dessus le plein écran", s.over_fullscreen, true);
                add(m, ID_CAPTURE, "Visible dans les captures d'écran", s.in_capture, true);
                sep(m);
                add(m, ID_FPS30, "Économie (30 i/s)", s.fps == Fps::Eco30, true);
                add(m, ID_FPS60, "Fluide (60 i/s)", s.fps == Fps::Smooth60, true);
                add(m, ID_FPSMAX, &format!("Performance max ({:.0} i/s)", self.geo.refresh), s.fps == Fps::Max, true);
                sep(m);
                add(m, ID_AUTOSTART, "Lancer au démarrage de Windows", settings::autostart_enabled(), true);
                sub(root, m, "Réglages");
            }
            sep(root);
            add(root, ID_QUIT, "Quitter", false, true);
            Some(root)
        }
    }

    pub fn command(&mut self, id: usize) {
        let s = &mut self.settings;
        match id {
            ID_QUIT => unsafe {
                let _ = PostMessageW(Some(self.ctl), WM_CLOSE, WPARAM(0), LPARAM(0));
                return;
            },
            ID_PAUSE => s.paused = !s.paused,
            ID_CLEAR => self.sim.clear(),
            i if (ID_INTENSITY..ID_INTENSITY + INTENSITY_NAMES.len()).contains(&i) => s.intensity = i - ID_INTENSITY,
            i if (ID_OPACITY..ID_OPACITY + OPACITIES.len()).contains(&i) => s.opacity = OPACITIES[i - ID_OPACITY],
            ID_ACC_GROUND => {
                s.acc_ground = !s.acc_ground;
                if !s.acc_ground {
                    self.sim.clear_ground();
                }
            }
            ID_ACC_WINDOWS => {
                s.acc_windows = !s.acc_windows;
                if !s.acc_windows {
                    self.sim.drop_windows();
                }
            }
            ID_MELT => s.melt = !s.melt,
            ID_TASKBAR => {
                s.behind_taskbar = !s.behind_taskbar;
                self.sim.clear_ground();
                self.sim.set_geometry(self.geo.w as f32, self.geo.h as f32, &self.geo.monitors, self.settings.behind_taskbar);
            }
            ID_FULLSCREEN => s.over_fullscreen = !s.over_fullscreen,
            ID_CAPTURE => {
                s.in_capture = !s.in_capture;
                self.apply_capture();
            }
            ID_FPS30 => s.fps = Fps::Eco30,
            ID_FPS60 => s.fps = Fps::Smooth60,
            ID_FPSMAX => s.fps = Fps::Max,
            ID_AUTOSTART => settings::set_autostart(!settings::autostart_enabled()),
            _ => return,
        }
        self.settings.save();
        self.update_tray();
        self.update_visibility();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.remove_tray();
        if let Some(i) = self.icon.take() {
            unsafe {
                let _ = DestroyIcon(i);
            }
        }
    }
}

// ---------------------------------------------------------------- fenêtres

fn show_menu(hwnd: HWND) {
    // Le menu est construit puis l'emprunt est relâché AVANT la boucle modale.
    let Some(Some(menu)) = with_app(|a| a.build_menu()) else { return };
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if cmd.0 > 0 {
            with_app(|a| a.command(cmd.0 as usize));
        }
    }
}

pub unsafe extern "system" fn ctl_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let ev = (lp.0 as u32) & 0xFFFF;
            if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP || ev == WM_CONTEXTMENU {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            with_app(|a| a.on_timer(wp.0));
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            with_app(|a| a.schedule_rebuild());
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            if wp.0 as u32 == SPI_SETWORKAREA.0 {
                with_app(|a| a.schedule_rebuild());
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_WTSSESSION_CHANGE => {
            match wp.0 {
                WTS_SESSION_LOCK => with_app(|a| a.set_locked(true)),
                WTS_SESSION_UNLOCK => with_app(|a| a.set_locked(false)),
                _ => None,
            };
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            match wp.0 {
                PBT_APMSUSPEND => {
                    with_app(|a| a.set_suspended(true));
                }
                PBT_APMRESUMEAUTOMATIC => {
                    with_app(|a| a.set_suspended(false));
                }
                PBT_POWERSETTINGCHANGE if lp.0 != 0 => {
                    let ps = &*(lp.0 as *const windows::Win32::System::Power::POWERBROADCAST_SETTING);
                    if ps.PowerSetting == windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE && ps.DataLength >= 1 {
                        let state = ps.Data[0]; // 0 = éteint, 1 = allumé, 2 = atténué
                        with_app(|a| a.set_display_off(state == 0));
                    }
                }
                _ => {}
            }
            LRESULT(1)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                with_app(|a| a.remove_tray());
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer a redémarré : on remet l'icône.
            with_app(|a| a.add_tray());
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DPICHANGED => LRESULT(0), // la géométrie est gérée par le contrôleur
        WM_ERASEBKGND => LRESULT(1),
        // Fermeture demandée de l'extérieur (installateur, gestionnaire de
        // tâches) : on quitte proprement au lieu de détruire l'overlay seul.
        WM_CLOSE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Crée la fenêtre de contrôle (cachée) et l'overlay.
pub fn create_windows() -> windows::core::Result<(HWND, HWND)> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);

        let ctl_class = w!("SnowhushWin.Controller");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(ctl_proc),
            hInstance: hinst,
            lpszClassName: ctl_class,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(windows::core::Error::from_win32());
        }
        let ov_class = w!("SnowhushWin.Overlay");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(overlay_proc),
            hInstance: hinst,
            lpszClassName: ov_class,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(windows::core::Error::from_win32());
        }

        let ctl = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            ctl_class,
            w!("Snowhush"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )?;
        let overlay = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            ov_class,
            w!("Snowhush Overlay"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst),
            None,
        )?;
        SetLayeredWindowAttributes(overlay, COLORREF(0), 255, LWA_ALPHA)?;

        // Pause automatique : session verrouillée, écran éteint.
        let _ = windows::Win32::System::RemoteDesktop::WTSRegisterSessionNotification(
            ctl,
            windows::Win32::System::RemoteDesktop::NOTIFY_FOR_THIS_SESSION,
        );
        let _ = windows::Win32::System::Power::RegisterPowerSettingNotification(
            HANDLE(ctl.0),
            &windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        Ok((ctl, overlay))
    }
}
