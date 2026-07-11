//! macOS menu bar tray icon.
//!
//! A native menu bar presence that survives window close: start/stop a
//! recording, bring the window forward, or quit — all from the dropdown.
//! The tray is a pure remote control: it emits Tauri events (`menu:start-
//! recording` / `menu:stop-recording`) that the frontend RecordingView
//! consumes, reusing the existing start()/stop() flows. No second recording
//! code path, no permission bypass.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::audio::CaptureEngine;

const IDLE_ICON: &[u8] = include_bytes!("../icons/tray-idle.png");
const RECORDING_ICON: &[u8] = include_bytes!("../icons/tray-recording.png");

fn fmt_elapsed(ms: u64) -> String {
    let s = ms / 1000;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, sec)
    } else {
        format!("{:02}:{:02}", m, sec)
    }
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Build the idle menu: Start Recording / Show / Quit.
fn build_idle_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let start = MenuItem::with_id(app, "start", "Start Recording", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let show = MenuItem::with_id(app, "show", "Show LilNotes", true, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit LilNotes", true, None::<&str>)?;
    Menu::with_items(app, &[&start, &sep1, &show, &sep2, &quit])
}

/// Build the recording menu: Stop / status label / Show / Quit.
/// The status item handle is stored so the poller can update its text each
/// tick without rebuilding the whole menu.
fn build_recording_menu(
    app: &AppHandle,
    status: &Arc<Mutex<Option<MenuItem<Wry>>>>,
    elapsed_ms: u64,
) -> tauri::Result<Menu<Wry>> {
    let stop = MenuItem::with_id(app, "stop", "Stop Recording", true, None::<&str>)?;
    let status_item = MenuItem::with_id(
        app,
        "status",
        format!("Recording • {}", fmt_elapsed(elapsed_ms)),
        false,
        None::<&str>,
    )?;
    *status.lock().unwrap() = Some(status_item.clone());
    let sep1 = PredefinedMenuItem::separator(app)?;
    let show = MenuItem::with_id(app, "show", "Show LilNotes", true, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit LilNotes", true, None::<&str>)?;
    Menu::with_items(app, &[&stop, &status_item, &sep1, &show, &sep2, &quit])
}

/// Swap the tray icon + menu to match the new recording state.
fn set_recording(app: &AppHandle, status: &Arc<Mutex<Option<MenuItem<Wry>>>>, elapsed_ms: u64) {
    let menu = build_recording_menu(app, status, elapsed_ms);
    let icon = tauri::image::Image::from_bytes(RECORDING_ICON);
    if let (Ok(menu), Ok(icon)) = (menu, icon) {
        if let Some(tray) = app.tray_by_id("lilnotes") {
            let _ = tray.set_menu(Some(menu));
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_icon_as_template(true);
        }
    }
}

fn set_idle(app: &AppHandle) {
    let menu = build_idle_menu(app);
    let icon = tauri::image::Image::from_bytes(IDLE_ICON);
    if let (Ok(menu), Ok(icon)) = (menu, icon) {
        if let Some(tray) = app.tray_by_id("lilnotes") {
            let _ = tray.set_menu(Some(menu));
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_icon_as_template(true);
        }
    }
}

pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    let handle = app.handle();

    // Build the initial menu/icon to match any recording already in progress.
    let engine = handle.state::<CaptureEngine>();
    let initial = engine.status();
    let status: Arc<Mutex<Option<MenuItem<Wry>>>> = Arc::new(Mutex::new(None));

    let (menu, icon_bytes) = if let Some((_, elapsed)) = &initial {
        let m = build_recording_menu(handle, &status, *elapsed)?;
        (m, RECORDING_ICON)
    } else {
        let m = build_idle_menu(handle)?;
        (m, IDLE_ICON)
    };

    let icon = tauri::image::Image::from_bytes(icon_bytes)?;
    TrayIconBuilder::with_id("lilnotes")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("LilNotes")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app)?;

    // Menu events: start/stop emit to the frontend, show just focuses the
    // window, quit exits the process.
    app.on_menu_event(move |app, event| match event.id().as_ref() {
        "start" => {
            show_window(app);
            let _ = app.emit_to("main", "menu:start-recording", ());
        }
        "stop" => {
            show_window(app);
            let _ = app.emit_to("main", "menu:stop-recording", ());
        }
        "show" => show_window(app),
        "quit" => app.exit(0),
        _ => {}
    });

    // 1 Hz poller: rebuild menu + swap icon on state change, update the
    // status item text on every tick while recording. Detached thread,
    // same pattern as the capture-levels emitter in audio/mod.rs.
    let poll_handle = handle.clone();
    let poll_status = status.clone();
    let mut was_recording = initial.is_some();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let engine = poll_handle.state::<CaptureEngine>();
        let current = engine.status();
        let is_recording = current.is_some();

        if is_recording != was_recording {
            if is_recording {
                let (_, elapsed) = current.as_ref().unwrap();
                set_recording(&poll_handle, &poll_status, *elapsed);
            } else {
                set_idle(&poll_handle);
            }
            was_recording = is_recording;
        }

        if is_recording {
            if let Some((_, elapsed)) = current {
                if let Ok(guard) = poll_status.lock() {
                    if let Some(item) = guard.as_ref() {
                        let _ = item.set_text(format!("Recording • {}", fmt_elapsed(elapsed)));
                    }
                }
            }
        }
    });

    Ok(())
}
