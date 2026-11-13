//! Snowhush pour Windows — neige sur le bureau, légère et transparente aux clics.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod icon;
mod log;
mod render;
mod settings;
mod sim;
mod sprites;
mod tracker;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::{CreateMutexW, INFINITE};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

fn main() {
    log::install_panic_hook();
    // Utilisé par le script de l'installateur : snowhush.exe --export-icon <fichier.ico>
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() == 3 && args[1] == "--export-icon" {
        let ok = icon::write_ico(std::path::Path::new(&args[2])).is_ok();
        std::process::exit(if ok { 0 } else { 1 });
    }
    unsafe {
        // Une seule instance.
        let _mutex = match CreateMutexW(None, true, w!("Local\\SnowhushWin.SingleInstance")) {
            Ok(h) if GetLastError() != ERROR_ALREADY_EXISTS => h,
            Ok(_) => return,
            Err(e) => {
                log::write(&format!("mutex : {e:?}"));
                return;
            }
        };
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let (ctl, overlay) = match app::create_windows() {
            Ok(v) => v,
            Err(e) => {
                log::write(&format!("création des fenêtres : {e:?}"));
                return;
            }
        };
        app::install(app::App::new(ctl, overlay));
        log::write("démarrage");
        run_loop();
        app::uninstall();
        let _ = DestroyWindow(overlay);
        log::write("arrêt");
    }
}

/// Boucle principale : attend soit un message, soit que la swapchain soit
/// prête pour l'image suivante. En pause, attend uniquement les messages
/// (0 % CPU).
unsafe fn run_loop() {
    let mut msg = MSG::default();
    loop {
        let wait = app::with_app(|a| if a.running() { a.waitable() } else { None }).flatten();
        let r = match wait {
            Some(h) => MsgWaitForMultipleObjectsEx(Some(&[h]), 250, QS_ALLINPUT, MWMO_INPUTAVAILABLE),
            None => MsgWaitForMultipleObjectsEx(None, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE),
        };
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // Image prête (ou délai dépassé : on dessine quand même pour ne
        // jamais rester bloqué si le signal se perd).
        if wait.is_some() && (r == WAIT_OBJECT_0 || r == WAIT_TIMEOUT) {
            app::with_app(|a| {
                if a.running() {
                    a.frame();
                }
            });
        }
    }
}
