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
  /** Whether a live transcription worker is attached. */
  liveTranscription: boolean;
  /** Set when live transcription was requested but couldn't start. */
  liveTranscriptionError: string | null;
}

export interface StoppedRecording {
  sessionId: string;
  micWav: string;
  systemWav: string;
  durationMs: number;
  startedAtMs: number;
  /** Present when live transcription ran; null when it was off. */
  segments: TranscriptSegment[] | null;
  transcriptionError: string | null;
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

// ---------------------------------------------------------------------------
// Transcription (M3)
// ---------------------------------------------------------------------------

export interface TranscriptSegment {
  /** "mic" (Me) or "system" (remote speakers). */
  source: "mic" | "system";
  startMs: number;
  endMs: number;
  text: string;
}

export interface SegmentEvent extends TranscriptSegment {
  sessionId: string;
}

export interface AppSettings {
  asrModel: string;
  liveTranscription: boolean;
}

export interface AsrModelInfo {
  id: string;
  label: string;
  approxBytes: number;
  note: string;
  downloaded: boolean;
  active: boolean;
}

export interface DownloadProgress {
  id: string;
  downloaded: number;
  total: number | null;
  done: boolean;
  error: string | null;
}

export function getSettings(): Promise<AppSettings> {
  return invoke<AppSettings>("get_settings");
}

export function updateSettings(newSettings: AppSettings): Promise<void> {
  return invoke("update_settings", { newSettings });
}

export function listAsrModels(): Promise<AsrModelInfo[]> {
  return invoke<AsrModelInfo[]>("list_asr_models");
}

/** Resolves when the download completes, fails, or is cancelled. */
export function downloadAsrModel(id: string): Promise<void> {
  return invoke("download_asr_model", { id });
}

export function cancelModelDownload(id: string): Promise<boolean> {
  return invoke<boolean>("cancel_model_download", { id });
}

/** Batch-transcribe a finished session; emits asr:segment events as it runs. */
export function transcribeSession(
  sessionId: string,
  micWav: string,
  systemWav: string,
): Promise<TranscriptSegment[]> {
  return invoke<TranscriptSegment[]>("transcribe_session", {
    sessionId,
    micWav,
    systemWav,
  });
}

export function onAsrSegment(cb: (e: SegmentEvent) => void): Promise<UnlistenFn> {
  return listen<SegmentEvent>("asr:segment", (ev) => cb(ev.payload));
}

export function onAsrDone(cb: (sessionId: string) => void): Promise<UnlistenFn> {
  return listen<{ sessionId: string }>("asr:done", (ev) => cb(ev.payload.sessionId));
}

export function onModelProgress(
  cb: (e: DownloadProgress) => void,
): Promise<UnlistenFn> {
  return listen<DownloadProgress>("model:progress", (ev) => cb(ev.payload));
}
