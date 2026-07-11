import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Pause, Play, Rewind, FastForward } from "lucide-react";

import { Button } from "@/components/ui/button";

interface Props {
  /** Filesystem path to the mic ("Me") WAV. */
  micWav: string;
  /** Filesystem path to the system (remote speakers) WAV. */
  systemWav: string;
  /** Seek request from a transcript timestamp click. A new object (new `n`)
   * re-triggers the seek even when `ms` is unchanged, so clicking the same
   * segment twice replays from that point. */
  seek: { ms: number; n: number } | null;
  /** Called with the leader's playback position (ms from recording start) as
   * it advances, so the transcript can highlight + follow the active group. */
  onTimeUpdate?: (currentMs: number) => void;
}

const SKIP_S = 10;

function fmtSecs(s: number): string {
  if (!Number.isFinite(s) || s < 0) s = 0;
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${String(sec).padStart(2, "0")}`;
}

/**
 * Plays the meeting's mic + system WAVs simultaneously over one shared
 * timeline. Both files share t=0 = recording start, so a segment's startMs
 * maps to the same offset in both. The channel with the longer duration is
 * the "leader" — its timeupdate drives the scrubber, so the timeline stays
 * accurate after the shorter channel ends.
 */
export default function AudioPlayer({ micWav, systemWav, seek, onTimeUpdate }: Props) {
  const micRef = useRef<HTMLAudioElement>(null);
  const systemRef = useRef<HTMLAudioElement>(null);

  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  // Which audio element drives the scrubber. Set once both durations are known.
  const [leader, setLeader] = useState<"mic" | "system" | null>(null);

  const micSrc = convertFileSrc(micWav);
  const systemSrc = convertFileSrc(systemWav);

  // Pick the longer channel as leader once both have reported their duration.
  const onLoadedMetadata = () => {
    const mic = micRef.current;
    const sys = systemRef.current;
    if (!mic || !sys) return;
    if (!Number.isFinite(mic.duration) || !Number.isFinite(sys.duration)) return;
    if (mic.duration <= 0 || sys.duration <= 0) return;
    setDuration(Math.max(mic.duration, sys.duration));
    setLeader(mic.duration >= sys.duration ? "mic" : "system");
  };

  const both = (): [HTMLAudioElement | null, HTMLAudioElement | null] => [
    micRef.current,
    systemRef.current,
  ];

  const play = async () => {
    const [mic, sys] = both();
    try {
      await Promise.all([mic?.play(), sys?.play()]);
    } catch {
      // Autoplay restriction or load failure — ignore; user can press play.
    }
  };

  const pause = () => {
    micRef.current?.pause();
    systemRef.current?.pause();
  };

  const toggle = () => (isPlaying ? pause() : play());

  const seekBoth = (secs: number) => {
    const clamped = Math.max(0, Math.min(secs, duration || secs));
    for (const el of both()) {
      if (el) el.currentTime = clamped;
    }
    setCurrentTime(clamped);
  };

  const skip = (delta: number) => {
    const base =
      (leader === "mic" ? micRef.current : systemRef.current)?.currentTime ?? currentTime;
    seekBoth(base + delta);
  };

  // Segment-click seek: a new seek object (new `n`) jumps both channels and
  // plays. Deps on the object reference so repeated clicks on the same segment
  // (same ms, new n) still replay.
  useEffect(() => {
    if (!seek) return;
    const secs = seek.ms / 1000;
    for (const el of both()) {
      if (el) el.currentTime = secs;
    }
    setCurrentTime(secs);
    // Propagate the seek position to the parent immediately so the
    // transcript row highlight moves on click, not after `play()`
    // resolves and the first `timeupdate` event fires (which is the
    // browser audio warmup latency when paused).
    onTimeUpdate?.(seek.ms);
    play();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seek]);

  // Pause on unmount so audio doesn't keep playing after navigating away.
  useEffect(() => {
    return () => {
      micRef.current?.pause();
      systemRef.current?.pause();
    };
  }, []);

  const scrubDisabled = duration <= 0;

  return (
    <div className="flex shrink-0 items-center gap-2 border-b bg-card/40 px-4 py-2">
      <Button
        size="icon"
        variant="ghost"
        onClick={toggle}
        disabled={duration <= 0}
        title={isPlaying ? "Pause" : "Play"}
        className="size-8"
      >
        {isPlaying ? <Pause className="size-4" /> : <Play className="size-4" />}
      </Button>
      <Button
        size="icon"
        variant="ghost"
        onClick={() => skip(-SKIP_S)}
        disabled={scrubDisabled}
        title={`Back ${SKIP_S}s`}
        className="size-8"
      >
        <Rewind className="size-4" />
      </Button>

      <input
        type="range"
        min={0}
        max={duration || 0}
        step={0.1}
        value={Math.min(currentTime, duration || 0)}
        disabled={scrubDisabled}
        onChange={(e) => seekBoth(Number(e.target.value))}
        className="h-1 min-w-0 flex-1 cursor-pointer accent-primary disabled:cursor-not-allowed"
        aria-label="Seek"
      />

      <Button
        size="icon"
        variant="ghost"
        onClick={() => skip(SKIP_S)}
        disabled={scrubDisabled}
        title={`Forward ${SKIP_S}s`}
        className="size-8"
      >
        <FastForward className="size-4" />
      </Button>

      <span className="shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground">
        {fmtSecs(currentTime)} / {fmtSecs(duration)}
      </span>

      {/* Hidden audio elements. The leader's timeupdate drives the scrubber. */}
      <audio
        ref={micRef}
        src={micSrc}
        preload="metadata"
        onLoadedMetadata={onLoadedMetadata}
        onTimeUpdate={() => {
          if (leader !== "mic") return;
          const t = micRef.current?.currentTime ?? 0;
          setCurrentTime(t);
          onTimeUpdate?.(t * 1000);
        }}
        onPlay={() => setIsPlaying(true)}
        onPause={() => {
          // Only flip to "paused" when BOTH are paused (they start together).
          if (systemRef.current?.paused) setIsPlaying(false);
        }}
        onEnded={() => {
          if (leader === "mic") setIsPlaying(false);
        }}
      />
      <audio
        ref={systemRef}
        src={systemSrc}
        preload="metadata"
        onLoadedMetadata={onLoadedMetadata}
        onTimeUpdate={() => {
          if (leader !== "system") return;
          const t = systemRef.current?.currentTime ?? 0;
          setCurrentTime(t);
          onTimeUpdate?.(t * 1000);
        }}
        onPlay={() => setIsPlaying(true)}
        onPause={() => {
          if (micRef.current?.paused) setIsPlaying(false);
        }}
        onEnded={() => {
          if (leader === "system") setIsPlaying(false);
        }}
      />
    </div>
  );
}
