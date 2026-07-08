import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, Mic, Square } from "lucide-react";

import { Button } from "@/components/ui/button";
import LevelMeter from "@/components/LevelMeter";
import {
  micPermissionStatus,
  onLevels,
  openPrivacySettings,
  recordingStatus,
  requestMicPermission,
  startRecording,
  stopRecording,
  type LevelsEvent,
  type PermissionStatus,
  type StoppedRecording,
} from "@/lib/ipc";
import { cn } from "@/lib/utils";
import type { Route } from "@/App";

interface Props {
  onNavigate: (route: Route) => void;
}

type Phase = "idle" | "starting" | "recording" | "stopping";

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
 * Recording view: record/stop, dual level meters (mic + system), elapsed
 * timer. The live transcript pane arrives with milestone 3.
 */
export default function RecordingView(_props: Props) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [levels, setLevels] = useState<LevelsEvent | null>(null);
  const [micPerm, setMicPerm] = useState<PermissionStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [finished, setFinished] = useState<StoppedRecording | null>(null);
  const phaseRef = useRef(phase);
  phaseRef.current = phase;

  // Restore state if a recording is already running (e.g. view remounted).
  useEffect(() => {
    recordingStatus().then((s) => {
      if (s.recording) setPhase("recording");
    });
    micPermissionStatus().then(setMicPerm);
  }, []);

  // Level meter events.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onLevels((e) => {
      if (phaseRef.current === "recording" || phaseRef.current === "starting") {
        setLevels(e);
      }
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  const start = useCallback(async () => {
    setError(null);
    setFinished(null);

    // Make sure the mic prompt happens before capture starts, so a denial is
    // explainable rather than a silent failure.
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
      await startRecording(); // may block on the system-audio TCC prompt
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
      setPhase("idle");
    } catch (e) {
      setPhase("idle");
      setError(String(e));
    }
  }, []);

  const recording = phase === "recording";

  return (
    <div className="mx-auto flex h-full max-w-xl flex-col items-center justify-center gap-8 p-8">
      {/* Permission banner */}
      {(micPerm === "denied" || micPerm === "restricted") && (
        <div className="flex w-full items-start gap-3 rounded-lg border border-destructive/30 bg-card p-4 text-sm">
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
          <div className="space-y-2">
            <p>
              LilNotes needs microphone access to record your side of the
              meeting. Enable it in System Settings, then come back.
            </p>
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

      {/* Timer */}
      <div
        className={cn(
          "font-mono text-5xl font-light tabular-nums tracking-tight",
          recording ? "text-foreground" : "text-muted-foreground/50",
        )}
      >
        {fmtElapsed(levels?.elapsedMs ?? 0)}
      </div>

      {/* Record / stop */}
      <button
        onClick={recording ? stop : start}
        disabled={phase === "starting" || phase === "stopping"}
        className={cn(
          "flex size-20 items-center justify-center rounded-full shadow-md transition-all",
          "focus-visible:ring-4 focus-visible:ring-ring/40 focus-visible:outline-none",
          "disabled:opacity-60",
          recording
            ? "bg-recording text-white hover:opacity-90"
            : "bg-primary text-primary-foreground hover:opacity-90",
        )}
        aria-label={recording ? "Stop recording" : "Start recording"}
      >
        {recording ? (
          <Square className="size-7 fill-current" />
        ) : (
          <Mic className="size-8" />
        )}
      </button>
      <p className="-mt-4 text-xs text-muted-foreground">
        {phase === "idle" && "Records your mic and system audio as separate tracks"}
        {phase === "starting" &&
          "Starting capture… approve the system-audio prompt if one appears"}
        {phase === "recording" && "Recording — click to stop"}
        {phase === "stopping" && "Finishing up…"}
      </p>

      {/* Meters */}
      <div
        className={cn(
          "w-full space-y-3 rounded-xl border bg-card p-5 transition-opacity",
          recording ? "opacity-100" : "opacity-40",
        )}
      >
        <LevelMeter label="Mic" rms={levels?.micRms ?? 0} peak={levels?.micPeak ?? 0} />
        <LevelMeter
          label="System"
          rms={levels?.systemRms ?? 0}
          peak={levels?.systemPeak ?? 0}
        />
      </div>

      {/* Result / error */}
      {finished && (
        <div className="w-full space-y-1.5 rounded-lg border bg-card p-4 text-sm">
          <div className="flex items-center gap-2 font-medium">
            <CheckCircle2 className="size-4 text-green-600" />
            Saved {fmtElapsed(finished.durationMs)} of audio
          </div>
          <p className="text-xs text-muted-foreground" data-selectable>
            {finished.micWav}
          </p>
          <p className="text-xs text-muted-foreground" data-selectable>
            {finished.systemWav}
          </p>
          <p className="pt-1 text-xs text-muted-foreground">
            Transcription arrives in milestone 3 — for now, verify both WAVs
            play and stay separate.
          </p>
        </div>
      )}
      {error && (
        <div className="w-full rounded-lg border border-destructive/30 bg-card p-4 text-sm text-destructive">
          <span data-selectable>{error}</span>
        </div>
      )}
    </div>
  );
}
