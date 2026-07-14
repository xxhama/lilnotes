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

/** Check whether the encrypted database file exists (no Keychain access). */
export function dbExists(): Promise<boolean> {
  return invoke<boolean>("db_exists");
}

/** Open the encrypted database (triggers the macOS Keychain prompt). */
export function initDb(): Promise<void> {
  return invoke<void>("init_db");
}

// ---------------------------------------------------------------------------
// Recording (M2)
// ---------------------------------------------------------------------------

export interface StartedRecording {
  sessionId: string;
  startedAtMs: number;
  /** The meeting row created at recording start; notes save against this id. */
  meetingId: number;
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

/** Tray "Start Recording" — pure signal, no payload. */
export function onMenuStartRecording(cb: () => void): Promise<UnlistenFn> {
  return listen("menu:start-recording", () => cb());
}

/** Tray "Stop Recording" — pure signal, no payload. */
export function onMenuStopRecording(cb: () => void): Promise<UnlistenFn> {
  return listen("menu:stop-recording", () => cb());
}

// ---------------------------------------------------------------------------
// Permissions (M2)
// ---------------------------------------------------------------------------

export type PermissionStatus = "granted" | "denied" | "undetermined" | "restricted";

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

export function openPrivacySettings(section: "microphone" | "systemAudio"): Promise<void> {
  return invoke("open_privacy_settings", { section });
}

// ---------------------------------------------------------------------------
// Transcription (M3)
// ---------------------------------------------------------------------------

export interface TranscriptSegment {
  /** DB row id; 0 for live/ephemeral segments before they're persisted. Used
   * to address mark-as-echo / delete mutations on persisted segments. */
  id: number;
  /** "mic" (Me) or "system" (remote speakers). */
  source: "mic" | "system";
  startMs: number;
  endMs: number;
  text: string;
  /** "Me" for mic; "SPEAKER_xx" after diarization; null until then. */
  speaker: string | null;
  /** "speech" (normal) or "echo" — a mic region the user marked as echo (the
   * ASR mis-attributed speaker echo to "Me"). Echo segments are hidden from the
   * transcript + summary but retained for offline echo re-processing. */
  kind?: "speech" | "echo";
  /** Soft-delete flag — hidden from transcript + summary, recoverable. */
  deleted?: boolean;
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
  /** Summary backend: "native" (built-in llama.cpp) or "ollama". */
  summaryBackend: string;
  /** Model tag/id for summaries within the active backend; null = auto-pick. */
  summaryModel: string | null;
  /** Custom summary prompt template; null = built-in default. */
  summaryTemplate: string | null;
  /** Cosine score at/above which a persona is a strong (pre-filled) suggestion. */
  personaAutoThreshold: number;
  /** Cosine score at/above which a persona is a tentative suggestion. */
  personaSuggestThreshold: number;
  /** Cosine cutoff for live persona auto-identification during recording. */
  personaLiveThreshold: number;
  /** Max voiceprints kept per persona (oldest pruned on enroll). 0 = unlimited. */
  voiceprintGalleryCap: number;
  /** Software acoustic echo cancellation using the system-audio feed as
   *  reference. Disable when using headphones. */
  aecEnabled: boolean;
  /** AEC aggressiveness preset. Stronger = less speaker echo but may slightly
   *  dull the user's own voice. Only meaningful when aecEnabled. */
  aecAggressiveness: AecAggressiveness;
  /** Whether the first-launch onboarding wizard has been completed. */
  onboardingComplete: boolean;
}

/** Live AEC aggressiveness preset (serde lowercase). */
export type AecAggressiveness = "balanced" | "strong" | "maximum";

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

/** Re-transcribe an existing meeting with a different whisper model, then
 * re-diarize. Does not change the global/live model. `modelId` must be a
 * downloaded model (the per-meeting dropdown pick). Returns the regenerated
 * diarized transcript. */
export function retranscribeMeeting(
  meetingId: number,
  modelId: string,
): Promise<DiarizedTranscript> {
  return invoke<DiarizedTranscript>("retranscribe_meeting", { meetingId, modelId });
}

export function onAsrSegment(cb: (e: SegmentEvent) => void): Promise<UnlistenFn> {
  return listen<SegmentEvent>("asr:segment", (ev) => cb(ev.payload));
}

export function onAsrDone(cb: (sessionId: string) => void): Promise<UnlistenFn> {
  return listen<{ sessionId: string }>("asr:done", (ev) => cb(ev.payload.sessionId));
}

export function onModelProgress(cb: (e: DownloadProgress) => void): Promise<UnlistenFn> {
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
  /** Path to an offline echo-cleaned mic WAV, if `cleanEcho` has been run. The
   * UI / AudioPlayer / re-transcribe prefer this over `micWav`. */
  micCleanedWav: string | null;
  notes: string | null;
  /** Epoch ms of the last notes save; null until notes have ever been saved. */
  notesUpdatedAtMs: number | null;
  segments: TranscriptSegment[];
  /** raw label -> user-chosen display name. */
  renames: Record<string, string>;
  /** raw label -> persona link (suggestion/confirmed) per meeting. */
  speakerLinks: Record<string, SpeakerLink>;
  speakerCount: number;
  /** Customer (account) this meeting belongs to; null = unassigned. */
  customerId: number | null;
  /** Segments hidden from the default transcript (echo-marked or
   * soft-deleted). Drives the "Show N hidden" toggle. */
  hiddenSegmentCount: number;
  /** Whisper model id that produced this transcript (e.g. "large-v3-turbo"),
   * recorded at transcription time. Null for meetings transcribed before this
   * was tracked. Shown as "Transcribed with: <label>" and used as the default
   * for the re-transcribe dropdown. */
  asrModel: string | null;
}

export function listMeetings(search?: string): Promise<MeetingSummary[]> {
  return invoke<MeetingSummary[]>("list_meetings", { search: search ?? null });
}

export function getMeeting(meetingId: number): Promise<MeetingDetail> {
  return invoke<MeetingDetail>("get_meeting", { meetingId });
}

/** Every segment for a meeting including echo-marked + soft-deleted ones, for
 * the "show hidden" transcript toggle. */
export function listHiddenSegments(meetingId: number): Promise<TranscriptSegment[]> {
  return invoke<TranscriptSegment[]>("list_hidden_segments", { meetingId });
}

/** Mark a mic segment the ASR mis-attributed to "Me" as echo (hides it from the
 * transcript + summary; retained for offline echo re-processing). */
export function markSegmentEcho(segmentId: number): Promise<void> {
  return invoke("mark_segment_echo", { segmentId });
}

/** Revert an echo mark back to normal speech. */
export function unmarkSegmentEcho(segmentId: number): Promise<void> {
  return invoke("unmark_segment_echo", { segmentId });
}

/** Soft-delete a segment (hide from transcript + summary, recoverable). */
export function deleteSegment(segmentId: number): Promise<void> {
  return invoke("delete_segment", { segmentId });
}

/** Restore a soft-deleted segment. */
export function restoreSegment(segmentId: number): Promise<void> {
  return invoke("restore_segment", { segmentId });
}

// ---------------------------------------------------------------------------
// Offline echo re-processing (Tier 3)
// ---------------------------------------------------------------------------

/** Payload of the `offline_aec:progress` event — drives the "Clean echo" bar. */
export interface OfflineAecProgress {
  meetingId: number;
  /** 0..=1 fraction of the offline AEC pass completed. */
  pct: number;
}

/**
 * Run offline AEC on a meeting's `mic.wav` using `system.wav` as the exact echo
 * reference, seeded by the user's echo-marked mic segments, then re-transcribe
 * the cleaned mic and re-diarize. Emits `offline_aec:progress` events as the
 * filter runs. The original `mic.wav` is preserved; the cleaned path is stored
 * as `meeting.micCleanedWav` so the action is revertible.
 */
export function cleanEcho(meetingId: number): Promise<DiarizedTranscript> {
  return invoke<DiarizedTranscript>("clean_echo", { meetingId });
}

/** Run offline echo cancellation on a single mic segment `[startMs, endMs]`.
 * Learns the echo path from marked echo regions and applies the clean only to
 * the selected region — audio outside it is left untouched. Re-transcribes +
 * re-diarizes, same as `cleanEcho`. Emits `offline_aec:progress` (same event,
 * keyed by `meetingId`) so the existing progress bar drives this too. */
export function cleanEchoSegment(
  meetingId: number,
  startMs: number,
  endMs: number,
): Promise<DiarizedTranscript> {
  return invoke<DiarizedTranscript>("clean_echo_segment", { meetingId, startMs, endMs });
}

/** Revert an offline echo clean: drop `micCleanedWav` and re-transcribe from the
 * original `mic.wav`. */
export function revertEchoClean(meetingId: number): Promise<DiarizedTranscript> {
  return invoke<DiarizedTranscript>("revert_echo_clean", { meetingId });
}

export function onOfflineAecProgress(cb: (e: OfflineAecProgress) => void): Promise<UnlistenFn> {
  return listen<OfflineAecProgress>("offline_aec:progress", (ev) => cb(ev.payload));
}

export function updateMeetingTitle(meetingId: number, title: string): Promise<void> {
  return invoke("update_meeting_title", { meetingId, title });
}

/** Persist freeform markdown notes for a meeting (empty string clears them). */
export function updateMeetingNotes(meetingId: number, notes: string): Promise<void> {
  return invoke("update_meeting_notes", { meetingId, notes });
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
// Built-in LLM models (Qwen3.5 GGUF via llama.cpp)
// ---------------------------------------------------------------------------

export interface NativeLlmModelInfo {
  id: string;
  label: string;
  approxBytes: number;
  note: string;
  downloaded: boolean;
  active: boolean;
}

export function listNativeModels(): Promise<NativeLlmModelInfo[]> {
  return invoke<NativeLlmModelInfo[]>("list_native_models");
}

/** Resolves when the download completes, fails, or is cancelled. */
export function downloadNativeModel(id: string): Promise<void> {
  return invoke("download_native_model", { id });
}

export function cancelNativeModelDownload(id: string): Promise<boolean> {
  return invoke<boolean>("cancel_native_model_download", { id });
}

/** Whether a model is currently loaded in GPU memory. */
export function nativeModelStatus(): Promise<boolean> {
  return invoke<boolean>("native_model_status");
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
  /** Short AI-generated title, or null if the title was left untouched. */
  title: string | null;
}

export interface SummaryToken {
  meetingId: number;
  token: string;
  isThinking: boolean;
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
export function summarizeMeeting(meetingId: number, model?: string): Promise<SummaryResult> {
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

export function onDiarizeProgress(cb: (e: DiarizeProgress) => void): Promise<UnlistenFn> {
  return listen<DiarizeProgress>("diarize:progress", (ev) => cb(ev.payload));
}

// ---------------------------------------------------------------------------
// Personas + voiceprints (M9)
// ---------------------------------------------------------------------------

export interface SpeakerLink {
  rawLabel: string;
  personaId: number | null;
  personaName: string | null;
  confidence: number | null;
  confirmed: boolean;
}

export interface Persona {
  id: number;
  displayName: string;
  notes: string | null;
  createdAtMs: number;
  updatedAtMs: number;
  voiceprintCount: number;
}

export type Tier = "auto" | "suggest" | "unknown";

export interface PersonaScore {
  personaId: number;
  displayName: string;
  score: number;
  tier: Tier;
}

export interface SpeakerMatch {
  rawLabel: string;
  suggestions: PersonaScore[];
  bestScore: number;
  alreadyLinked: boolean;
  confirmed: boolean;
}

export function listPersonas(): Promise<Persona[]> {
  return invoke<Persona[]>("list_personas");
}

export function createPersona(displayName: string): Promise<number> {
  return invoke<number>("create_persona", { displayName });
}

export function renamePersona(personaId: number, displayName: string): Promise<void> {
  return invoke<void>("rename_persona", { personaId, displayName });
}

export function deletePersona(personaId: number): Promise<void> {
  return invoke<void>("delete_persona", { personaId });
}

export function deleteAllVoiceprints(): Promise<void> {
  return invoke<void>("delete_all_voiceprints");
}

/** Re-run embedding + matching; emits speakers:identified. */
export function identifySpeakers(meetingId: number): Promise<SpeakerMatch[]> {
  return invoke<SpeakerMatch[]>("identify_speakers", { meetingId });
}

export function confirmSpeakerPersona(
  meetingId: number,
  rawLabel: string,
  personaId: number,
): Promise<void> {
  return invoke<void>("confirm_speaker_persona", { meetingId, rawLabel, personaId });
}

export function unlinkSpeakerPersona(meetingId: number, rawLabel: string): Promise<void> {
  return invoke<void>("unlink_speaker_persona", { meetingId, rawLabel });
}

export function onSpeakersIdentified(cb: (e: SpeakerMatch[]) => void): Promise<UnlistenFn> {
  return listen<SpeakerMatch[]>("speakers:identified", (ev) => cb(ev.payload));
}

/** Fires when a background voiceprint enrollment finishes (refresh counts). */
export function onVoiceprintsEnrolled(cb: () => void): Promise<UnlistenFn> {
  return listen("voiceprints:enrolled", () => cb());
}

// ---------------------------------------------------------------------------
// Customers (accounts)
// ---------------------------------------------------------------------------

export interface CustomerSummary {
  id: number;
  name: string;
  logo: string | null;
  meetingCount: number;
  lastMeetingAtMs: number | null;
}

export interface CustomerRosterEntry {
  personaId: number;
  displayName: string;
  meetingCount: number;
  lastSeenMs: number | null;
}

export interface CustomerRollupRow {
  id: number;
  model: string;
  content: string;
  createdAtMs: number;
  meetingCount: number;
}

export interface CustomerDetail {
  id: number;
  name: string;
  logo: string | null;
  notes: string | null;
  createdAtMs: number;
  updatedAtMs: number;
  meetingCount: number;
  lastMeetingAtMs: number | null;
  firstMeetingAtMs: number | null;
  totalDurationMs: number | null;
  personaRoster: CustomerRosterEntry[];
  meetings: MeetingSummary[];
  meetingsWithSummaryCount: number;
  latestRollup: CustomerRollupRow | null;
}

export interface CustomerSearchHit {
  field: string;
  snippet: string;
}

export interface CustomerSearchResult {
  meetingId: number;
  title: string;
  startedAtMs: number;
  hits: CustomerSearchHit[];
}

export interface CustomerSummaryResult {
  summaryId: number;
  model: string;
  content: string;
  meetingCount: number;
}

export interface CustomerSummaryToken {
  customerId: number;
  token: string;
  isThinking: boolean;
}

export function listCustomers(): Promise<CustomerSummary[]> {
  return invoke<CustomerSummary[]>("list_customers");
}

export function createCustomer(name: string, notes?: string): Promise<number> {
  return invoke<number>("create_customer", { name, notes: notes ?? null });
}

export function renameCustomer(customerId: number, name: string): Promise<void> {
  return invoke<void>("rename_customer", { customerId, name });
}

export function updateCustomerNotes(customerId: number, notes: string | null): Promise<void> {
  return invoke<void>("update_customer_notes", { customerId, notes });
}

export function getCustomer(customerId: number): Promise<CustomerDetail> {
  return invoke<CustomerDetail>("get_customer", { customerId });
}

export function deleteCustomer(customerId: number): Promise<void> {
  return invoke<void>("delete_customer", { customerId });
}

/** Reassign a meeting to a customer (or unassign with null). */
export function setMeetingCustomer(meetingId: number, customerId: number | null): Promise<void> {
  return invoke<void>("set_meeting_customer", { meetingId, customerId });
}

/** Merge source into target (irreversible); returns after source is deleted. */
export function mergeCustomers(sourceId: number, targetId: number): Promise<void> {
  return invoke<void>("merge_customers", { sourceId, targetId });
}

export function searchCustomerMeetings(
  customerId: number,
  query: string,
): Promise<CustomerSearchResult[]> {
  return invoke<CustomerSearchResult[]>("search_customer_meetings", {
    customerId,
    query,
  });
}

export function summarizeCustomer(
  customerId: number,
  model?: string,
): Promise<CustomerSummaryResult> {
  return invoke<CustomerSummaryResult>("summarize_customer", {
    customerId,
    model: model ?? null,
  });
}

export function listCustomerSummaries(customerId: number): Promise<CustomerRollupRow[]> {
  return invoke<CustomerRollupRow[]>("list_customer_summaries", { customerId });
}

export function onCustomerSummaryToken(cb: (e: CustomerSummaryToken) => void): Promise<UnlistenFn> {
  return listen<CustomerSummaryToken>("customer-summary:token", (ev) => cb(ev.payload));
}
