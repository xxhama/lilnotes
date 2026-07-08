/**
 * Typed wrappers around Tauri IPC.
 *
 * Every backend command gets a thin, typed function here so views never call
 * `invoke` with raw strings scattered around the codebase.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// Health check (M1)
// ---------------------------------------------------------------------------

/** Response of the `ping` health-check command. */
export interface PingResponse {
  /** Echo of the message that was sent. */
  echo: string;
  /** Backend crate version. */
  version: string;
  /** Unix epoch milliseconds when the backend handled the call. */
  handledAtMs: number;
}

/** Round-trip a message through the Rust backend. */
export function ping(message: string): Promise<PingResponse> {
  return invoke<PingResponse>("ping", { message });
}

// ---------------------------------------------------------------------------
// Recording (M2)
// ---------------------------------------------------------------------------

export interface StartedRecording {
  sessionId: string;
  startedAtMs: number;
}

export interface StoppedRecording {
  sessionId: string;
  micWav: string;
  systemWav: string;
  durationMs: number;
  startedAtMs: number;
}

export interface RecordingStatus {
  recording: boolean;
  sessionId: string | null;
  elapsedMs: number | null;
}

/** Payload of the `capture:levels` event (~10 Hz while recording). */
export interface LevelsEvent {
  sessionId: string;
  elapsedMs: number;
  micRms: number;
  micPeak: number;
  systemRms: number;
  systemPeak: number;
}

export function startRecording(): Promise<StartedRecording> {
  return invoke<StartedRecording>("start_recording");
}

export function stopRecording(): Promise<StoppedRecording> {
  return invoke<StoppedRecording>("stop_recording");
}

export function recordingStatus(): Promise<RecordingStatus> {
  return invoke<RecordingStatus>("recording_status");
}

export function onLevels(cb: (e: LevelsEvent) => void): Promise<UnlistenFn> {
  return listen<LevelsEvent>("capture:levels", (ev) => cb(ev.payload));
}

// ---------------------------------------------------------------------------
// Permissions (M2)
// ---------------------------------------------------------------------------

export type PermissionStatus =
  | "granted"
  | "denied"
  | "undetermined"
  | "restricted";

export function micPermissionStatus(): Promise<PermissionStatus> {
  return invoke<PermissionStatus>("mic_permission_status");
}

/** Shows the mic TCC prompt if undetermined; resolves when answered. */
export function requestMicPermission(): Promise<boolean> {
  return invoke<boolean>("request_mic_permission");
}

/**
 * Probe system-audio access by briefly creating a Core Audio process tap.
 * Triggers the "System Audio Recording" prompt on first use.
 */
export function probeSystemAudioPermission(): Promise<boolean> {
  return invoke<boolean>("probe_system_audio_permission");
}

export function openPrivacySettings(
  section: "microphone" | "systemAudio",
): Promise<void> {
  return invoke("open_privacy_settings", { section });
}
