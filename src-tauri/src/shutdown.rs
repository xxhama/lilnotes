//! Graceful shutdown: resource cleanup + crash recovery.
//!
//! Centralizes cleanup logic so no resource leaks when the app quits or
//! crashes:
//!
//! - `on_run_event` — dispatched from `lib.rs`'s `App::run` callback. Handles
//!   `ExitRequested` (confirm dialog if recording in progress) and `Exit`
//!   (defensive cleanup: kill sidecar, stop recording, WAL checkpoint).
//! - `stop_recording_and_finalize` — replicates `commands::stop_recording`
//!   synchronously, callable from the shutdown path.
//! - `panic_cleanup` — installed as `std::panic::set_hook`; kills the
//!   llama-server sidecar (the expensive GPU resource) so a panic doesn't
//!   orphan it.
//!
//! All state access in the `Exit` path uses `try_state` + `catch_unwind` —
//! if a mutex is poisoned from a prior panic, we skip that resource rather
//! than panicking again during cleanup.

use std::sync::Arc;

use tauri::{AppHandle, ExitRequestApi, Manager, RunEvent};
use tauri_plugin_dialog::DialogExt;

use crate::asr::{AsrEngine, Segment};
use crate::audio::CaptureEngine;
use crate::commands::AsrSession;
use crate::db::LazyDb;
use crate::mcp::McpServer;
use crate::summary::sidecar::SidecarLlmClient;

/// Dispatched from `lib.rs`'s `App::run` callback for every run event.
pub fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        RunEvent::ExitRequested { api, .. } => {
            handle_exit_requested(app, &api);
        }
        RunEvent::Exit => {
            // Never panic in the Exit handler — the process is about to die
            // and a panic here would mask the original error.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cleanup_on_exit(app);
            }));
        }
        _ => {}
    }
}

/// On exit request: if a recording is in progress, prevent the exit and ask
/// the user whether to stop and save. If confirmed, the recording is
/// finalized on a background thread and `app.exit(0)` is re-triggered.
/// If cancelled, the app stays alive and the recording continues.
/// If no recording is in progress, the exit proceeds normally.
fn handle_exit_requested(app: &AppHandle, api: &ExitRequestApi) {
    let Some(engine) = app.try_state::<CaptureEngine>() else {
        return;
    };
    if !engine.is_recording() {
        return;
    }

    api.prevent_exit();
    let handle = app.clone();
    app.dialog()
        .message("A recording is in progress. Stop and save it, then quit?")
        .title("Recording in progress")
        .show(move |confirmed| {
            if !confirmed {
                return;
            }
            std::thread::spawn(move || {
                match stop_recording_and_finalize(&handle) {
                    Ok(()) => eprintln!("[shutdown] recording finalized before quit"),
                    Err(e) => eprintln!("[shutdown] error finalizing recording: {e}"),
                }
                handle.exit(0);
            });
        });
}

/// Stop the active recording, join the ASR worker, and finalize the DB row.
/// Replicates `commands::stop_recording` but synchronous — callable from
/// the shutdown path without the async IPC wrapper.
fn stop_recording_and_finalize(app: &AppHandle) -> Result<(), String> {
    let engine = app.state::<CaptureEngine>();
    let stopped = engine.stop()?;

    // Join the ASR worker (if present) and collect segments. The worker
    // exits when the capture pipelines drop (which `engine.stop()` just
    // did), so the join should return promptly.
    let segments = if let Some(asr_session) = app.try_state::<AsrSession>() {
        let handle = asr_session.0.lock().unwrap().take();
        match handle {
            Some(h) => match h.join() {
                Ok(Ok(segs)) => segs,
                Ok(Err(e)) => {
                    eprintln!("[shutdown] ASR worker error: {e}");
                    Vec::new()
                }
                Err(_) => {
                    eprintln!("[shutdown] ASR worker panicked");
                    Vec::new()
                }
            },
            None => Vec::<Segment>::new(),
        }
    } else {
        Vec::new()
    };

    // Finalize the DB meeting row created at recording start.
    let db = app.state::<Arc<LazyDb>>();
    let meeting_id = db
        .meeting_id_by_session(&stopped.session_id)?
        .ok_or_else(|| "no meeting row for this session".to_string())?;
    db.finalize_meeting(
        meeting_id,
        (stopped.started_at_ms + stopped.duration_ms) as i64,
        &stopped.mic_wav,
        &stopped.system_wav,
    )?;
    if !segments.is_empty() {
        db.replace_segments(meeting_id, &segments)?;
    }

    eprintln!("[shutdown] recording saved: meeting {meeting_id}");
    Ok(())
}

/// Defensive cleanup on `RunEvent::Exit`. Called right before the process
/// exits — covers all exit paths (tray quit, Cmd+Q, system logout). Best-effort:
/// each step is independent and errors are logged, never propagated.
fn cleanup_on_exit(app: &AppHandle) {
    // 0. Stop the MCP listener so agents get a clean connection-refused
    //    rather than a hung request, and the port is free for a relaunch.
    if let Some(mcp) = app.try_state::<McpServer>() {
        mcp.stop();
    }

    // 1. Kill llama-server sidecar (frees GPU memory). This is the most
    //    important step — without it the child process outlives the app.
    if let Some(sidecar) = app.try_state::<SidecarLlmClient>() {
        if let Err(e) = sidecar.unload(app) {
            eprintln!("[shutdown] error killing sidecar: {e}");
        }
    }

    // 1b. Drop the whisper model (context + state, ~700 MB incl. the Metal
    //     backend). The idle-unload watcher would free it eventually, but
    //     exit should release GPU memory promptly. In-process state, so no
    //     orphaned child to worry about — just drop the LoadedModel.
    if let Some(asr) = app.try_state::<Arc<AsrEngine>>() {
        asr.unload();
    }

    // 2. Stop recording if still active (defensive — covers system-forced
    //    exit that bypassed the ExitRequested dialog).
    if let Some(engine) = app.try_state::<CaptureEngine>() {
        if engine.is_recording() {
            eprintln!("[shutdown] stopping recording on exit (defensive)");
            match stop_recording_and_finalize(app) {
                Ok(()) => {}
                Err(e) => eprintln!("[shutdown] error stopping recording: {e}"),
            }
        }
    }

    // 3. Best-effort WAL checkpoint — keeps the WAL file from growing
    //    unbounded across sessions. SQLite auto-checkpoints at 1000 frames
    //    by default, but an explicit TRUNCATE on exit reclaims the space.
    //    Guard with `get()` — the DB may not be initialized yet if the user
    //    quits during onboarding before calling `init_db`.
    if let Some(lazy_db) = app.try_state::<Arc<LazyDb>>() {
        if let Some(db) = lazy_db.get() {
            if let Err(e) = db.wal_checkpoint() {
                eprintln!("[shutdown] WAL checkpoint error: {e}");
            }
        }
    }
}
