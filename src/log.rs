//! Journal minimal (%LOCALAPPDATA%\SnowhushWin\snowhush.log), plafonné à 256 Ko.

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("SnowhushWin").join("snowhush.log"))
}

pub fn write(msg: &str) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&p).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&p, p.with_extension("old.log"));
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        write(&format!("PANIC : {info}"));
    }));
}
