//! TCC permission helpers.
//!
//! - Microphone: proper status query + request via AVCaptureDevice.
//! - System audio ("System Audio Recording Only" under Screen & System Audio
//!   Recording): there is no public status-query API, so we probe by creating
//!   a short-lived process tap, which also surfaces the system prompt on
//!   first use. NOTE: the prompt only fires for signed binaries; `tauri dev`
//!   builds are ad-hoc signed, which normally suffices, but if no prompt
//!   appears run a bundled build once (see README troubleshooting).

use serde::Serialize;

use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStatus {
    Granted,
    Denied,
    Undetermined,
    Restricted,
}

pub fn mic_status() -> PermissionStatus {
    // SAFETY: AVMediaTypeAudio is a valid static; class method is thread-safe.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(AVMediaTypeAudio) };
    match status {
        AVAuthorizationStatus::Authorized => PermissionStatus::Granted,
        AVAuthorizationStatus::Denied => PermissionStatus::Denied,
        AVAuthorizationStatus::Restricted => PermissionStatus::Restricted,
        _ => PermissionStatus::Undetermined,
    }
}

/// Trigger the mic permission prompt; blocks until the user answers.
/// Call from a non-main thread (Tauri async commands are fine).
pub fn request_mic_access() -> bool {
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    let block = block2::RcBlock::new(move |granted: objc2::runtime::Bool| {
        let _ = tx.send(granted.as_bool());
    });
    // SAFETY: valid media type + completion block.
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(AVMediaTypeAudio, &block);
    }
    rx.recv().unwrap_or(false)
}

/// Open System Settings at the relevant privacy pane.
pub fn open_privacy_settings(section: &str) -> Result<(), String> {
    let anchor = match section {
        "microphone" => "Privacy_Microphone",
        "systemAudio" => "Privacy_AudioCapture",
        _ => return Err(format!("unknown settings section: {section}")),
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|e| format!("could not open System Settings: {e}"))?;
    Ok(())
}
