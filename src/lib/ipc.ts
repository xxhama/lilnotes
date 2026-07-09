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
  /** The persisted meeting created for this session. */
  meetingId: number;
  sessionId: string;
  durationMs: number;
  /** Segments captured live (empty when live transcription was off). */
  segments: TranscriptSegment[];
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
  /** "Me" for mic; "SPEAKER_xx" after diarization; null until then. */
  speaker: string | null;
}

export interface SegmentEvent extends TranscriptSegment {
  sessionId: string;
}

export interface AppSettings {
  asrModel: string;
  liveTranscription: boolean;
  /** Where recordings are stored; null = app data dir. */
  storageDir: string | null;
  /** Delete WAVs once a meeting is transcribed + diarized. */
  deleteAudioAfterTranscription: boolean;
  /** Ollama model tag for summaries; null = auto-pick from installed. */
  summaryModel: string | null;
  /** Custom summary prompt template; null = built-in default. */
  summaryTemplate: string | null;
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

/** Batch-transcribe a persisted meeting; emits asr:segment events as it runs. */
export function transcribeMeeting(meetingId: number): Promise<TranscriptSegment[]> {
  return invoke<TranscriptSegment[]>("transcribe_meeting", { meetingId });
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

// ---------------------------------------------------------------------------
// Diarization (M4)
// ---------------------------------------------------------------------------

export interface DiarizedTranscript {
  segments: TranscriptSegment[];
  /** Number of distinct speakers found on the system channel. */
  speakerCount: number;
  /** True if the WAVs were removed per the delete-audio setting. */
  audioDeleted: boolean;
}

export interface DiarizeProgress {
  processed: number;
  total: number;
}

/**
 * Diarize a meeting's system channel, persist the labeled segments, and
 * (per settings) delete the audio afterwards. First use downloads two
 * small models (~34 MB, `model:progress` events).
 *
 * `numSpeakers`: pass the exact remote-speaker count when known — much
 * more reliable than automatic estimation.
 */
export function diarizeMeeting(
  meetingId: number,
  numSpeakers?: number,
): Promise<DiarizedTranscript> {
  return invoke<DiarizedTranscript>("diarize_meeting", {
    meetingId,
    numSpeakers: numSpeakers ?? null,
  });
}

// ---------------------------------------------------------------------------
// Meetings (M5)
// ---------------------------------------------------------------------------

export interface MeetingSummary {
  id: number;
  title: string;
  startedAtMs: number;
  durationMs: number | null;
  segmentCount: number;
  speakerCount: number;
  preview: string | null;
  hasAudio: boolean;
}

export interface MeetingDetail {
  id: number;
  sessionId: string;
  title: string;
  startedAtMs: number;
  endedAtMs: number | null;
  micWav: string | null;
  systemWav: string | null;
  notes: string | null;
  segments: TranscriptSegment[];
  /** raw label -> user-chosen display name. */
  renames: Record<string, string>;
}

export function listMeetings(search?: string): Promise<MeetingSummary[]> {
  return invoke<MeetingSummary[]>("list_meetings", { search: search ?? null });
}

export function getMeeting(meetingId: number): Promise<MeetingDetail> {
  return invoke<MeetingDetail>("get_meeting", { meetingId });
}

export function updateMeetingTitle(meetingId: number, title: string): Promise<void> {
  return invoke("update_meeting_title", { meetingId, title });
}

/** Persist a display name for a raw speaker label (null clears it). */
export function renameSpeaker(
  meetingId: number,
  rawLabel: string,
  displayName: string | null,
): Promise<void> {
  return invoke("rename_speaker", { meetingId, rawLabel, displayName });
}

/** Delete a meeting and its audio files. */
export function deleteMeeting(meetingId: number): Promise<void> {
  return invoke("delete_meeting", { meetingId });
}

// ---------------------------------------------------------------------------
// Ollama + summaries (M6)
// ---------------------------------------------------------------------------

export interface OllamaStatus {
  reachable: boolean;
  version: string | null;
}

export interface InstalledOllamaModel {
  name: string;
  sizeBytes: number;
  parameterSize: string | null;
  family: string | null;
}

export interface OllamaModels {
  installed: InstalledOllamaModel[];
  /** The model summaries will use (settings override or auto-pick). */
  active: string | null;
}

export interface SuggestedModel {
  tag: string;
  tier: string;
  approxDownload: string;
  note: string;
  installed: boolean;
  /** True when the matching -mlx (Apple MLX runtime) variant is installed. */
  mlxInstalled: boolean;
}

export interface PullProgress {
  model: string;
  status: string;
  completed: number;
  total: number;
  done: boolean;
  error: string | null;
}

export interface SummaryRow {
  id: number;
  model: string;
  content: string;
  createdAtMs: number;
}

export interface SummaryResult {
  summaryId: number;
  model: string;
  content: string;
}

export interface SummaryToken {
  meetingId: number;
  token: string;
}

export function ollamaStatus(): Promise<OllamaStatus> {
  return invoke<OllamaStatus>("ollama_status");
}

export function listOllamaModels(): Promise<OllamaModels> {
  return invoke<OllamaModels>("list_ollama_models");
}

export function suggestedOllamaModels(): Promise<SuggestedModel[]> {
  return invoke<SuggestedModel[]>("suggested_ollama_models");
}

/** Resolves when the pull completes/fails; progress via `ollama:pull`. */
export function pullOllamaModel(model: string): Promise<void> {
  return invoke("pull_ollama_model", { model });
}

export function cancelOllamaPull(model: string): Promise<boolean> {
  return invoke<boolean>("cancel_ollama_pull", { model });
}

export function defaultSummaryTemplate(): Promise<string> {
  return invoke<string>("default_summary_template");
}

/** Generate + persist a summary; tokens stream via `summary:token`. */
export function summarizeMeeting(
  meetingId: number,
  model?: string,
): Promise<SummaryResult> {
  return invoke<SummaryResult>("summarize_meeting", {
    meetingId,
    model: model ?? null,
  });
}

export function listSummaries(meetingId: number): Promise<SummaryRow[]> {
  return invoke<SummaryRow[]>("list_summaries", { meetingId });
}

export function onSummaryToken(cb: (e: SummaryToken) => void): Promise<UnlistenFn> {
  return listen<SummaryToken>("summary:token", (ev) => cb(ev.payload));
}

export function onOllamaPull(cb: (e: PullProgress) => void): Promise<UnlistenFn> {
  return listen<PullProgress>("ollama:pull", (ev) => cb(ev.payload));
}

export function onDiarizeProgress(
  cb: (e: DiarizeProgress) => void,
): Promise<UnlistenFn> {
  return listen<DiarizeProgress>("diarize:progress", (ev) => cb(ev.payload));
}
