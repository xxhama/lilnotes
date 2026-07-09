import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, FileText, Loader2, Mic, Square } from "lucide-react";

import { Button } from "@/components/ui/button";
import LevelMeter from "@/components/LevelMeter";
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
  type LevelsEvent,
  type PermissionStatus,
  type StoppedRecording,
  type TranscriptSegment,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";
import { cn } from "@/lib/utils";
import type { Route } from "@/App";

interface Props {
  onNavigate: (route: Route) => void;
}

type Phase =
  | "idle"
  | "starting"
  | "recording"
  | "stopping"
  | "transcribing"
  | "diarizing";

function fmtElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(sec).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

/**
 * Recording view: record/stop, dual level meters, elapsed timer, live
 * transcript. When a recording finishes (and is transcribed + diarized),
 * navigation moves to the persisted meeting's detail view.
 */
export default function RecordingView({ onNavigate }: Props) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [levels, setLevels] = useState<LevelsEvent | null>(null);
  const [micPerm, setMicPerm] = useState<PermissionStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [finished, setFinished] = useState<StoppedRecording | null>(null);
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [live, setLive] = useState(false);
  const phaseRef = useRef(phase);
  phaseRef.current = phase;

  // Restore state if a recording is already running (e.g. view remounted).
  useEffect(() => {
    recordingStatus().then((s) => {
      if (s.recording) setPhase("recording");
    });
    micPermissionStatus().then(setMicPerm);
  }, []);

  useTauriEvent(onLevels, (e) => {
    if (phaseRef.current === "recording" || phaseRef.current === "starting") {
      setLevels(e);
    }
  });
  useTauriEvent(onAsrSegment, (e) => {
    setSegments((prev) => [...prev, e]);
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
      if (started.liveTranscriptionError) {
        setNotice(`Recording without live transcript: ${started.liveTranscriptionError}`);
      }
      setPhase("recording");
    } catch (e) {
      setPhase("idle");
      setError(String(e));
    }
  }, []);

  const stop = useCallback(async () => {
    setPhase("stopping");
    try {
      const result = await stopRecording();
      setFinished(result);
      setLevels(null);
      if (result.transcriptionError) {
        setNotice(`Transcription problem: ${result.transcriptionError}`);
      }
      if (result.segments.length > 0) {
        setSegments(result.segments);
        setPhase("diarizing");
        try {
          await diarizeMeeting(result.meetingId);
        } catch (e) {
          setNotice(`Speaker identification failed: ${e}`);
        }
        onNavigate({ name: "meeting", meetingId: String(result.meetingId) });
      } else {
        setPhase("idle");
      }
    } catch (e) {
      setPhase("idle");
      setError(String(e));
    }
  }, [onNavigate]);

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

  const recording = phase === "recording";
  const showTranscript =
    segments.length > 0 || recording || phase === "transcribing" || phase === "diarizing";

  return (
    <div className="flex h-full flex-col">
      {/* Top: controls */}
      <div className="mx-auto flex w-full max-w-xl flex-col items-center gap-5 p-6 pb-4">
        {(micPerm === "denied" || micPerm === "restricted") && (
          <div className="flex w-full items-start gap-3 rounded-lg border border-destructive/30 bg-card p-4 text-sm">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
            <div className="space-y-2">
              <p>LilNotes needs microphone access to record your side of the meeting.</p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => openPrivacySettings("microphone")}
              >
                Open System Settings
              </Button>
            </div>
          </div>
        )}

        <div className="flex w-full items-center justify-center gap-8">
          <div
            className={cn(
              "font-mono text-4xl font-light tabular-nums tracking-tight",
              recording ? "text-foreground" : "text-muted-foreground/50",
            )}
          >
            {fmtElapsed(levels?.elapsedMs ?? 0)}
          </div>

          <button
            onClick={recording ? stop : start}
            disabled={!["idle", "recording"].includes(phase)}
            className={cn(
              "flex size-16 items-center justify-center rounded-full shadow-md transition-all",
              "focus-visible:ring-4 focus-visible:ring-ring/40 focus-visible:outline-none",
              "disabled:opacity-60",
              recording
                ? "bg-recording text-white hover:opacity-90"
                : "bg-primary text-primary-foreground hover:opacity-90",
            )}
            aria-label={recording ? "Stop recording" : "Start recording"}
          >
            {recording ? (
              <Square className="size-6 fill-current" />
            ) : (
              <Mic className="size-7" />
            )}
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
            (live ? "Recording — transcript fills in below" : "Recording")}
          {phase === "stopping" && "Finishing up… flushing the last transcript chunk"}
          {phase === "transcribing" && "Transcribing recording…"}
          {phase === "diarizing" && (
            <span className="inline-flex items-center gap-1.5">
              <Loader2 className="size-3.5 animate-spin" />
              Identifying speakers… (first run downloads a small model)
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
              onClick={() =>
                onNavigate({ name: "meeting", meetingId: String(finished.meetingId) })
              }
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

      {/* Bottom: live transcript */}
      {showTranscript && (
        <div className="min-h-0 flex-1 border-t bg-card/50">
          {(phase === "transcribing" || phase === "diarizing") && segments.length === 0 ? (
            <div className="flex items-center justify-center gap-2 p-6 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
              {phase === "transcribing" ? "Transcribing…" : "Identifying speakers…"}
            </div>
          ) : (
            <TranscriptPane
              segments={segments}
              follow={recording || phase === "transcribing"}
              className="h-full"
            />
          )}
        </div>
      )}
    </div>
  );
}
