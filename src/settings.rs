//! Réglages persistants (%APPDATA%\SnowhushWin\settings.ini) et lancement
//! au démarrage (clé Run de HKCU).

use std::path::PathBuf;
use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::*;

pub const INTENSITY_NAMES: [&str; 4] = ["Légère", "Moyenne", "Forte", "Tempête"];
pub const INTENSITY_RATES: [f32; 4] = [3.0, 7.0, 14.0, 28.0];
pub const OPACITIES: [u8; 6] = [25, 40, 55, 70, 85, 100];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fps {
    Eco30,
    Smooth60,
    Max,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub intensity: usize,
    pub opacity: u8,
    pub fps: Fps,
    pub acc_ground: bool,
    pub acc_windows: bool,
    pub melt: bool,
    pub over_fullscreen: bool,
    pub in_capture: bool,
    pub behind_taskbar: bool,
    pub paused: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            intensity: 1,
            opacity: 85,
            fps: Fps::Smooth60,
            acc_ground: true,
            acc_windows: true,
            melt: true,
            over_fullscreen: false,
            in_capture: false,
            behind_taskbar: false,
            paused: false,
        }
    }
}

pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("SnowhushWin"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings::default();
        let Some(path) = data_dir().map(|d| d.join("settings.ini")) else { return s };
        let Ok(text) = std::fs::read_to_string(&path) else { return s };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let b = v == "1" || v.eq_ignore_ascii_case("true");
            match k {
                "intensity" => s.intensity = v.parse::<usize>().unwrap_or(1).min(INTENSITY_RATES.len() - 1),
                "opacity" => s.opacity = v.parse::<u8>().unwrap_or(85).clamp(10, 100),
                "fps" => {
                    s.fps = match v {
                        "30" => Fps::Eco30,
                        "max" => Fps::Max,
                        _ => Fps::Smooth60,
                    }
                }
                "acc_ground" => s.acc_ground = b,
                "acc_windows" => s.acc_windows = b,
                "melt" => s.melt = b,
                "over_fullscreen" => s.over_fullscreen = b,
                "in_capture" => s.in_capture = b,
                "behind_taskbar" => s.behind_taskbar = b,
                "paused" => s.paused = b,
                _ => {}
            }
        }
        s
    }

    /// Écriture atomique (fichier temporaire puis renommage).
    pub fn save(&self) {
        let Some(dir) = data_dir() else { return };
        let b = |v: bool| if v { 1 } else { 0 };
        let fps = match self.fps {
            Fps::Eco30 => "30",
            Fps::Smooth60 => "60",
            Fps::Max => "max",
        };
        let text = format!(
            "intensity={}\nopacity={}\nfps={}\nacc_ground={}\nacc_windows={}\nmelt={}\nover_fullscreen={}\nin_capture={}\nbehind_taskbar={}\npaused={}\n",
            self.intensity,
            self.opacity,
            fps,
            b(self.acc_ground),
            b(self.acc_windows),
            b(self.melt),
            b(self.over_fullscreen),
            b(self.in_capture),
            b(self.behind_taskbar),
            b(self.paused)
        );
        let res = std::fs::create_dir_all(&dir).and_then(|_| {
            let tmp = dir.join("settings.ini.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, dir.join("settings.ini"))
        });
        if let Err(e) = res {
            crate::log::write(&format!("sauvegarde des réglages impossible : {e}"));
        }
    }
}

// ------------------------------------------------------------ démarrage auto

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("SnowhushWin");

fn exe_command() -> Option<Vec<u16>> {
    let exe = std::env::current_exe().ok()?;
    let s = format!("\"{}\"", exe.display());
    Some(s.encode_utf16().chain(std::iter::once(0)).collect())
}

fn read_run_value() -> Option<Vec<u16>> {
    unsafe {
        let mut size: u32 = 0;
        let r = RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size));
        if r.is_err() || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        if r.is_err() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        buf.truncate(len);
        buf.push(0);
        Some(buf)
    }
}

pub fn autostart_enabled() -> bool {
    read_run_value().is_some()
}

pub fn set_autostart(on: bool) {
    unsafe {
        let res = if on {
            match exe_command() {
                Some(cmd) => {
                    let bytes = std::slice::from_raw_parts(cmd.as_ptr() as *const u8, cmd.len() * 2);
                    RegSetKeyValueW(
                        HKEY_CURRENT_USER,
                        RUN_KEY,
                        RUN_VALUE,
                        REG_SZ.0,
                        Some(bytes.as_ptr() as *const _),
                        bytes.len() as u32,
                    )
                }
                None => return,
            }
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE)
        };
        if res.is_err() && on {
            crate::log::write(&format!("démarrage auto : échec registre {:?}", res));
        }
    }
}

/// Si l'exécutable a été déplacé, met à jour le chemin enregistré.
pub fn refresh_autostart_path() {
    if let (Some(cur), Some(exe)) = (read_run_value(), exe_command()) {
        if cur != exe {
            set_autostart(true);
        }
    }
}
