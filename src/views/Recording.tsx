/**
 * Recording view: record/stop, dual level meters, elapsed timer, live
 * transcript on the right, notes on the left. When a recording finishes
 * (and is transcribed + diarized), navigation moves to the persisted
 * meeting's detail view. Kept always mounted by App.tsx (hidden via CSS
 * when inactive) so recording state survives navigation away and back.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, Check, FileText, Loader2, Mic, Pencil, Square } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import LevelMeter from "@/components/LevelMeter";
import NotesEditor from "@/components/NotesEditor";
import TranscriptPane from "@/components/TranscriptPane";
import {
  diarizeMeeting,
  micPermissionStatus,
  onAsrSegment,
  onLevels,
  openPrivacySettings,
  recordingStatus,
  requestMicPermission,
  startRecording,
  stopRecording,
  transcribeMeeting,
  updateMeetingNotes,
  type LevelsEvent,
  type PermissionStatus,
  type StoppedRecording,
  type TranscriptSegment,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";
import { cn } from "@/lib/utils";
import type { Route } from "@/App";

interface Props {
  /** True when the Record route is the current route. */
  active: boolean;
  onNavigate: (route: Route) => void;
  /** Tray-triggered auto-start signal; consumed once then cleared. */
  autoStart?: boolean;
  /** Tray-triggered auto-stop signal; consumed once then cleared. */
  autoStop?: boolean;
  onAutoStartHandled?: () => void;
  onAutoStopHandled?: () => void;
}

type Phase = "idle" | "starting" | "recording" | "stopping" | "transcribing" | "diarizing";

/** Minimum time the "Saving…" indicator stays visible so it doesn't flicker. */
const SAVE_MIN_VISIBLE_MS = 600;

function fmtElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(sec).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

function fmtSavedAt(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

/**
 * Recording view: record/stop, dual level meters, elapsed timer, live
 * transcript on the right, notes on the left. When a recording finishes
 * (and is transcribed + diarized), navigation moves to the persisted
 * meeting's detail view.
 */
export default function RecordingView({
  active,
  onNavigate,
  autoStart,
  autoStop,
  onAutoStartHandled,
  onAutoStopHandled,
}: Props) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [levels, setLevels] = useState<LevelsEvent | null>(null);
  const [micPerm, setMicPerm] = useState<PermissionStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [finished, setFinished] = useState<StoppedRecording | null>(null);
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [live, setLive] = useState(false);
  const [meetingId, setMeetingId] = useState<number | null>(null);
  const phaseRef = useRef(phase);
  phaseRef.current = phase;

  // Notes autosave state (mirrors MeetingDetail's debounce pattern).
  const [notesStatus, setNotesStatus] = useState<"modified" | "saving" | "saved">("saved");
  const [savedAtMs, setSavedAtMs] = useState<number | null>(null);
  const notesRef = useRef<string>("");
  const saveTimer = useRef<number | null>(null);
  const holdTimer = useRef<number | null>(null);
  const saveGen = useRef(0);

  // Restore state if a recording is already running (e.g. view remounted).
  useEffect(() => {
    recordingStatus().then((s) => {
      if (s.recording) setPhase("recording");
    });
    micPermissionStatus().then(setMicPerm);
  }, []);

  // Reset to fresh idle when the user returns to Record after a recording
  // has already completed and navigated away. Skipped while a recording
  // is actively in progress (starting/recording/stopping) so live state
  // is preserved across navigation.
  useEffect(() => {
    if (!active) return;
    const inProgress = ["starting", "recording", "stopping"].includes(phaseRef.current);
    if (inProgress) return;
    // Recording cycle already completed — reset for the next one.
    setPhase("idle");
    setFinished(null);
    setSegments([]);
    setLive(false);
    setMeetingId(null);
    setError(null);
    setNotice(null);
    setNotesStatus("saved");
    setSavedAtMs(null);
    notesRef.current = "";
    if (saveTimer.current) {
      clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    if (holdTimer.current) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
  }, [active]);

  useTauriEvent(onLevels, (e) => {
    if (phaseRef.current === "recording" || phaseRef.current === "starting") {
      setLevels(e);
    }
  });
  useTauriEvent(onAsrSegment, (e) => {
    setSegments((prev) =>
      // Belt-and-braces: never render the same segment twice even if an
      // event is delivered more than once.
      prev.some((s) => s.source === e.source && s.startMs === e.startMs && s.text === e.text)
        ? prev
        : [...prev, e],
    );
  });

  const start = useCallback(async () => {
    setError(null);
    setNotice(null);
    setFinished(null);
    setSegments([]);

    let perm = await micPermissionStatus();
    if (perm === "undetermined") {
      const granted = await requestMicPermission();
      perm = granted ? "granted" : "denied";
      setMicPerm(perm);
    }
    if (perm === "denied" || perm === "restricted") {
      setMicPerm(perm);
      setError("Microphone access is denied.");
      return;
    }

    setPhase("starting");
    try {
      const started = await startRecording(); // may block on the TCC prompt
      setLive(started.liveTranscription);
      setMeetingId(started.meetingId);
      if (started.liveTranscriptionError) {
        setNotice(`Recording without live transcript: ${started.liveTranscriptionError}`);
      }
      setPhase("recording");
    } catch (e) {
      setPhase("idle");
      setError(String(e));
    }
  }, []);

  const flushNotes = useCallback(async () => {
    if (meetingId == null) return;
    if (saveTimer.current) {
      clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    if (holdTimer.current) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
    ++saveGen.current;
    try {
      await updateMeetingNotes(meetingId, notesRef.current);
      setSavedAtMs(Date.now());
      setNotesStatus("saved");
    } catch {
      setNotesStatus("modified");
    }
  }, [meetingId]);

  const stop = useCallback(async () => {
    setPhase("stopping");
    try {
      await flushNotes();
      const result = await stopRecording();
      setFinished(result);
      setLevels(null);
      if (result.transcriptionError) {
        setNotice(`Transcription problem: ${result.transcriptionError}`);
      }
      // Always land on Meeting Detail after stop. If a live transcript was
      // produced, Meeting Detail auto-starts speaker identification there
      // (autoDiarize) — the spinner moves off the Record page. If live
      // transcription was off, Meeting Detail shows a "Transcribe" button.
      onNavigate({
        name: "meeting",
        meetingId: String(result.meetingId),
        autoDiarize: result.segments.length > 0,
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setPhase("idle");
    }
  }, [onNavigate, flushNotes]);

  // Tray-triggered start/stop: consume the one-shot signal from the menu bar
  // dropdown and drive the existing start()/stop() flows. The in-flight ref
  // guards prevent StrictMode's double-invoke from calling start()/stop()
  // twice before the phase state updates.
  const onAutoStartHandledRef = useRef(onAutoStartHandled);
  onAutoStartHandledRef.current = onAutoStartHandled;
  const onAutoStopHandledRef = useRef(onAutoStopHandled);
  onAutoStopHandledRef.current = onAutoStopHandled;
  const startInFlightRef = useRef(false);
  const stopInFlightRef = useRef(false);

  useEffect(() => {
    if (!autoStart) return;
    onAutoStartHandledRef.current?.();
    if (startInFlightRef.current) return;
    if (phaseRef.current === "recording" || phaseRef.current === "starting") return;
    startInFlightRef.current = true;
    start().finally(() => {
      startInFlightRef.current = false;
    });
  }, [autoStart, start]);

  useEffect(() => {
    if (!autoStop) return;
    onAutoStopHandledRef.current?.();
    if (stopInFlightRef.current) return;
    if (phaseRef.current !== "recording") return;
    stopInFlightRef.current = true;
    stop().finally(() => {
      stopInFlightRef.current = false;
    });
  }, [autoStop, stop]);

  const runBatchTranscription = useCallback(async () => {
    if (!finished) return;
    setPhase("transcribing");
    setError(null);
    setSegments([]);
    try {
      await transcribeMeeting(finished.meetingId);
      setPhase("diarizing");
      try {
        await diarizeMeeting(finished.meetingId);
      } catch (e) {
        setNotice(`Speaker identification failed: ${e}`);
      }
      onNavigate({ name: "meeting", meetingId: String(finished.meetingId) });
    } catch (e) {
      setPhase("idle");
      setError(String(e));
    }
  }, [finished, onNavigate]);

  const onNotesActivity = useCallback(() => {
    if (saveTimer.current) clearTimeout(saveTimer.current);
    if (holdTimer.current) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
    ++saveGen.current;
    setNotesStatus("modified");
    saveTimer.current = window.setTimeout(async () => {
      saveTimer.current = null;
      const gen = saveGen.current;
      setNotesStatus("saving");
      const startedAt = Date.now();
      try {
        await updateMeetingNotes(meetingId!, notesRef.current);
        if (saveGen.current !== gen) return;
        setSavedAtMs(Date.now());
        const holdMs = Math.max(0, SAVE_MIN_VISIBLE_MS - (Date.now() - startedAt));
        holdTimer.current = window.setTimeout(() => {
          holdTimer.current = null;
          setNotesStatus("saved");
        }, holdMs);
      } catch {
        setNotesStatus("modified");
      }
    }, 1000);
  }, [meetingId]);

  const onNotesMarkdown = useCallback((md: string) => {
    notesRef.current = md;
  }, []);

  // Cancel any pending save/hold on unmount so they never fire against a stale id.
  useEffect(() => {
    return () => {
      if (saveTimer.current) clearTimeout(saveTimer.current);
      if (holdTimer.current) clearTimeout(holdTimer.current);
    };
  }, []);

  const recording = phase === "recording";
  const showTranscript =
    segments.length > 0 || recording || phase === "transcribing" || phase === "diarizing";
  // The two-pane workspace appears once a meeting row exists (recording
  // started). Notes persist against `meetingId`; the transcript pane
  // carries through stop → transcribe → diarize until navigation.
  const showWorkspace = meetingId != null;

  return (
    <div className="flex h-full flex-1 flex-col">
      {/* Top: controls */}
      <div className="mx-auto flex w-full max-w-xl flex-col items-center gap-5 p-6 pt-4 pb-4">
        {(micPerm === "denied" || micPerm === "restricted") && (
          <div className="flex w-full items-start gap-3 rounded-lg border border-destructive/30 bg-card p-4 text-sm">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
            <div className="space-y-2">
              <p>LilNotes needs microphone access to record your side of the meeting.</p>
              <Button variant="outline" size="sm" onClick={() => openPrivacySettings("microphone")}>
                Open System Settings
              </Button>
            </div>
          </div>
        )}

        <div className="flex w-full items-center justify-center gap-8">
          <div
            className={cn(
              "w-56 text-right font-mono text-4xl font-light tabular-nums tracking-tight",
              recording ? "text-foreground" : "text-muted-foreground/50",
            )}
          >
            {fmtElapsed(levels?.elapsedMs ?? 0)}
          </div>

          <button
            onClick={recording ? stop : start}
            disabled={!["idle", "recording"].includes(phase)}
            className={cn(
              "flex size-16 shrink-0 items-center justify-center rounded-full shadow-md transition-all",
              "focus-visible:ring-4 focus-visible:ring-ring/40 focus-visible:outline-none",
              "disabled:opacity-60",
              recording
                ? "bg-recording text-white hover:opacity-90"
                : "bg-primary text-primary-foreground hover:opacity-90",
            )}
            aria-label={recording ? "Stop recording" : "Start recording"}
          >
            {recording ? <Square className="size-6 fill-current" /> : <Mic className="size-7" />}
          </button>

          <div className="w-56 space-y-2">
            <LevelMeter label="Mic" rms={levels?.micRms ?? 0} peak={levels?.micPeak ?? 0} />
            <LevelMeter
              label="System"
              rms={levels?.systemRms ?? 0}
              peak={levels?.systemPeak ?? 0}
            />
          </div>
        </div>

        <p className="text-xs text-muted-foreground">
          {phase === "idle" && !finished && "Records mic and system audio as separate tracks"}
          {phase === "starting" && "Starting capture…"}
          {phase === "recording" &&
            (live
              ? "Recording — take notes on the left, transcript fills in on the right"
              : "Recording")}
          {phase === "stopping" && "Finishing up… flushing the last transcript chunk"}
          {phase === "transcribing" && "Transcribing recording…"}
          {phase === "diarizing" && (
            <span className="inline-flex items-center gap-1.5">
              <Loader2 className="size-3.5 animate-spin" />
              Identifying speakers…
            </span>
          )}
          {phase === "idle" && finished && `Saved ${fmtElapsed(finished.durationMs)} of audio`}
        </p>

        {finished && phase === "idle" && (
          <div className="flex gap-2">
            {finished.segments.length === 0 && (
              <Button size="sm" onClick={runBatchTranscription}>
                <FileText /> Transcribe recording
              </Button>
            )}
            <Button
              size="sm"
              variant="outline"
              onClick={() => onNavigate({ name: "meeting", meetingId: String(finished.meetingId) })}
            >
              View meeting
            </Button>
          </div>
        )}

        {notice && (
          <div className="w-full rounded-lg border bg-card p-3 text-xs text-muted-foreground">
            {notice}
          </div>
        )}
        {error && (
          <div className="w-full rounded-lg border border-destructive/30 bg-card p-3 text-sm text-destructive">
            <span data-selectable>{error}</span>
          </div>
        )}
      </div>

      {/* Bottom: notes (left) + transcript (right) */}
      {showWorkspace && (
        <div className="flex min-h-0 flex-1 border-t">
          {/* Notes pane */}
          <div className="flex min-h-0 flex-1 flex-col border-r">
            <div className="flex items-center justify-between gap-2 border-b bg-card/60 px-4 py-2">
              <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
                Notes
              </span>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span
                    key={notesStatus}
                    className={
                      "inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium animate-in fade-in duration-200 " +
                      (notesStatus === "saved"
                        ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                        : "bg-muted text-muted-foreground")
                    }
                  >
                    {notesStatus === "modified" ? (
                      <>
                        <Pencil className="size-3" />
                        Modified
                      </>
                    ) : notesStatus === "saving" ? (
                      <>
                        <Loader2 className="size-3 animate-spin" />
                        Saving…
                      </>
                    ) : (
                      <>
                        <Check className="size-3" />
                        Saved
                      </>
                    )}
                  </span>
                </TooltipTrigger>
                {notesStatus === "saved" && (
                  <TooltipContent>
                    {savedAtMs != null ? `Saved at ${fmtSavedAt(savedAtMs)}` : "Saved"}
                  </TooltipContent>
                )}
              </Tooltip>
            </div>
            <div className="min-h-0 flex-1">
              {meetingId != null && (
                <NotesEditor
                  key={meetingId}
                  initialValue=""
                  onChange={onNotesMarkdown}
                  onActivity={onNotesActivity}
                />
              )}
            </div>
          </div>

          {/* Transcript pane */}
          <div className="flex min-h-0 flex-1 flex-col bg-card/50">
            {(phase === "transcribing" || phase === "diarizing") && segments.length === 0 ? (
              <div className="flex items-center justify-center gap-2 p-6 text-sm text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
                {phase === "transcribing" ? "Transcribing…" : "Identifying speakers…"}
              </div>
            ) : showTranscript ? (
              <TranscriptPane
                segments={segments}
                follow={recording || phase === "transcribing"}
                className="h-full"
              />
            ) : (
              <div className="flex items-center justify-center p-6">
                <p className="text-xs text-muted-foreground">
                  Transcript will appear here as speech is recognized.
                </p>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
