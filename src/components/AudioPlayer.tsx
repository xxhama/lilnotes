import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { FastForward, Gauge, Loader2, Pause, Play, Rewind } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

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

const SKIP_S = 10; // transport skip + global ←/→/j/l
const FINE_SKIP_S = 5; // slider ↑/↓
const PAGE_SKIP_S = 15; // slider PageUp/Down
const RATES = [0.75, 1, 1.25, 1.5, 2];

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
 * the "leader" — its timeupdate drives the readout + transcript sync, so the
 * timeline stays accurate after the shorter channel ends.
 *
 * Mounted once at page level (above both the Review and Transcript tabs) so
 * playback + the player UI persist across tab switches. The bar is a single
 * row: transport cluster | seek bar | time readout + speed.
 */
export default function AudioPlayer({ micWav, systemWav, seek, onTimeUpdate }: Props) {
  const micRef = useRef<HTMLAudioElement>(null);
  const systemRef = useRef<HTMLAudioElement>(null);

  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0); // seconds (leader)
  const [duration, setDuration] = useState(0);
  // Which audio element drives the readout. Set once both durations are known.
  const [leader, setLeader] = useState<"mic" | "system" | null>(null);
  const [rate, setRate] = useState(1);
  const [waiting, setWaiting] = useState(false); // buffering/loading
  const [bufferedPct, setBufferedPct] = useState(0);
  // Hover/scrub time-preview position (0..100). Null when not hovering/dragging.
  const [previewPct, setPreviewPct] = useState<number | null>(null);
  // True only during an active scrub. Drives the enlarged thumb (a ref can't,
  // since dragging starts without a re-render). Only flips on down/up → ≤2
  // renders per drag. draggingRef (below) is the no-render mirror the rAF
  // tick reads each frame to skip visual updates while the pointer owns them.
  const [dragging, setDragging] = useState(false);

  // Visual refs driven directly (rAF while playing, pointer while dragging) so
  // the fill + thumb advance at 60fps without re-rendering React state.
  const fillRef = useRef<HTMLDivElement>(null);
  const thumbRef = useRef<HTMLDivElement>(null);
  const trackRef = useRef<HTMLDivElement>(null);
  const rafRef = useRef<number | null>(null);
  const draggingRef = useRef(false);

  const micSrc = convertFileSrc(micWav);
  const systemSrc = convertFileSrc(systemWav);

  const leaderEl = (): HTMLAudioElement | null =>
    leader === "mic" ? micRef.current : systemRef.current;

  const setVisuals = (pct: number) => {
    const p = `${Math.max(0, Math.min(100, pct))}%`;
    if (fillRef.current) fillRef.current.style.width = p;
    if (thumbRef.current) thumbRef.current.style.left = p;
  };

  // Pick the longer channel as leader once both have reported their duration.
  const onLoadedMetadata = () => {
    const mic = micRef.current;
    const sys = systemRef.current;
    if (!mic || !sys) return;
    if (!Number.isFinite(mic.duration) || !Number.isFinite(sys.duration)) return;
    if (mic.duration <= 0 || sys.duration <= 0) return;
    setDuration(Math.max(mic.duration, sys.duration));
    setLeader(mic.duration >= sys.duration ? "mic" : "system");
    // A new src resets playbackRate to 1 — re-apply the chosen rate.
    mic.playbackRate = rate;
    sys.playbackRate = rate;
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
    // Sync state to the exact leader position so the visuals land correctly
    // when the rAF loop stops.
    const el = leaderEl();
    if (el) setCurrentTime(el.currentTime);
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
    const base = leaderEl()?.currentTime ?? currentTime;
    seekBoth(base + delta);
  };

  // NOTE: setting `playbackRate` on a playing <audio> element causes a brief
  // ~200–300ms stop-and-resume on macOS (WKWebView). This is WebKit bug #163433
  // — AVFoundation reconfigures its pipeline on rate change, and the gap
  // reproduces even in native AVPlayer. It's unavoidable while preserving pitch
  // (the pitch-preserved path is what reconfigures); the only gapless option is
  // WebAudio AudioBufferSourceNode, which shifts pitch. We keep pitch preserved
  // and accept the pause. Don't re-investigate — see the plan file / bug 163433.
  const changeRate = (r: number) => {
    setRate(r);
    if (micRef.current) micRef.current.playbackRate = r;
    if (systemRef.current) systemRef.current.playbackRate = r;
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
    const mic = micRef.current;
    const sys = systemRef.current;
    return () => {
      mic?.pause();
      sys?.pause();
    };
  }, []);

  // rAF: while playing, advance the fill + thumb at 60fps directly via refs
  // (no React state churn). Skips while dragging so the pointer owns visuals.
  useEffect(() => {
    if (!isPlaying) return;
    const tick = () => {
      if (!draggingRef.current && leader && duration > 0) {
        const t = leaderEl()?.currentTime ?? 0;
        setVisuals((t / duration) * 100);
      }
      rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => {
      if (rafRef.current != null) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isPlaying, leader, duration]);

  // While paused (or before metadata), drive visuals from React state. While
  // playing, the rAF owns the refs — this early-returns so the ~4/sec
  // timeupdate state updates don't jitter the 60fps visuals.
  useEffect(() => {
    if (isPlaying) return;
    setVisuals(duration > 0 ? (currentTime / duration) * 100 : 0);
  }, [currentTime, duration, isPlaying]);

  // Page-level keyboard shortcuts. Bound once; the latest handlers are read
  // through a ref so we don't re-bind every render. Ignored while typing in a
  // text field (input/textarea/select or contenteditable, e.g. the Milkdown
  // notes editor, title input, customer picker).
  const kgRef = useRef({ toggle, skip });
  kgRef.current = { toggle, skip };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const ae = document.activeElement;
      const tag = ae?.tagName;
      if (
        tag === "INPUT" ||
        tag === "TEXTAREA" ||
        tag === "SELECT" ||
        (ae as HTMLElement)?.isContentEditable
      )
        return;
      if (duration <= 0) return;
      if (e.key === " " || e.key === "k") {
        e.preventDefault();
        kgRef.current.toggle();
      } else if (e.key === "j" || e.key === "ArrowLeft") {
        e.preventDefault();
        kgRef.current.skip(-SKIP_S);
      } else if (e.key === "l" || e.key === "ArrowRight") {
        e.preventDefault();
        kgRef.current.skip(SKIP_S);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [duration]);

  const scrubDisabled = duration <= 0;

  // --- Seek bar pointer + keyboard handlers ---
  const pctFromEvent = (clientX: number): number => {
    const track = trackRef.current;
    if (!track) return 0;
    const rect = track.getBoundingClientRect();
    if (rect.width <= 0) return 0;
    return Math.max(0, Math.min(100, ((clientX - rect.left) / rect.width) * 100));
  };

  const onTrackPointerDown = (e: React.PointerEvent) => {
    if (scrubDisabled) return;
    draggingRef.current = true;
    setDragging(true);
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    const pct = pctFromEvent(e.clientX);
    const secs = (pct / 100) * duration;
    setVisuals(pct);
    setPreviewPct(pct);
    setCurrentTime(secs); // live readout; audio seeks on release
  };

  const onTrackPointerMove = (e: React.PointerEvent) => {
    if (duration <= 0) return;
    const pct = pctFromEvent(e.clientX);
    setPreviewPct(pct);
    if (draggingRef.current) {
      const secs = (pct / 100) * duration;
      setVisuals(pct);
      setCurrentTime(secs); // live readout while dragging
    }
  };

  const onTrackPointerUp = (e: React.PointerEvent) => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    setDragging(false);
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    const pct = pctFromEvent(e.clientX);
    seekBoth((pct / 100) * duration); // commit seek to both channels
  };

  const onTrackPointerLeave = () => {
    if (!draggingRef.current) setPreviewPct(null);
  };

  const onTrackKeyDown = (e: React.KeyboardEvent) => {
    if (scrubDisabled) return;
    let secs = currentTime;
    let handled = true;
    if (e.key === "ArrowUp") secs = currentTime + FINE_SKIP_S;
    else if (e.key === "ArrowDown") secs = currentTime - FINE_SKIP_S;
    else if (e.key === "PageUp") secs = currentTime + PAGE_SKIP_S;
    else if (e.key === "PageDown") secs = currentTime - PAGE_SKIP_S;
    else if (e.key === "Home") secs = 0;
    else if (e.key === "End") secs = duration;
    else handled = false;
    if (!handled) return;
    e.preventDefault();
    e.stopPropagation(); // don't bubble to the window listener
    seekBoth(secs);
  };

  const previewSecs = previewPct != null && duration > 0 ? (previewPct / 100) * duration : null;

  return (
    // Bottom-docked bar — last row of the Meeting Detail flex column. Elevated
    // above the content via `border-t` + `bg-card` (no one-off colors). Tooltips
    // and the speed menu open upward (side="top") so they're never clipped by
    // the viewport bottom. `shrink-0` keeps the bar at h-16 while the Body
    // (flex-1) above absorbs the rest; the Body's internal scrollers end at the
    // bar's top, so nothing hides behind it.
    <div
      role="group"
      aria-label="Audio player"
      className="flex h-16 shrink-0 items-center gap-3 border-t border-border bg-card px-4"
    >
      {/* Zone 1 — transport cluster */}
      <div className="flex items-center gap-1">
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              size="icon"
              variant="ghost"
              onClick={() => skip(-SKIP_S)}
              disabled={scrubDisabled || waiting}
              aria-label="Skip back 10 seconds"
              className="size-9 flex-col gap-0"
            >
              <Rewind className="size-4" />
              <span className="text-[8px] font-semibold leading-none text-muted-foreground">
                10
              </span>
            </Button>
          </TooltipTrigger>
          <TooltipContent side="top">Back 10 seconds</TooltipContent>
        </Tooltip>

        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              size="icon"
              variant="ghost"
              onClick={toggle}
              disabled={scrubDisabled}
              aria-label={isPlaying ? "Pause" : "Play"}
              className="size-10 rounded-full bg-secondary text-secondary-foreground hover:bg-secondary/80"
            >
              {waiting && !isPlaying ? (
                <Loader2 className="size-5 animate-spin" />
              ) : isPlaying ? (
                <Pause className="size-5" />
              ) : (
                <Play className="size-5 translate-x-px" />
              )}
            </Button>
          </TooltipTrigger>
          <TooltipContent side="top">{isPlaying ? "Pause" : "Play"}</TooltipContent>
        </Tooltip>

        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              size="icon"
              variant="ghost"
              onClick={() => skip(SKIP_S)}
              disabled={scrubDisabled || waiting}
              aria-label="Skip forward 10 seconds"
              className="size-9 flex-col gap-0"
            >
              <FastForward className="size-4" />
              <span className="text-[8px] font-semibold leading-none text-muted-foreground">
                10
              </span>
            </Button>
          </TooltipTrigger>
          <TooltipContent side="top">Forward 10 seconds</TooltipContent>
        </Tooltip>
      </div>

      {/* Zone 2 — seek bar (custom slider; see component doc) */}
      <div
        ref={trackRef}
        role="slider"
        tabIndex={0}
        aria-label="Seek"
        aria-orientation="horizontal"
        aria-valuemin={0}
        aria-valuemax={Math.round(duration)}
        aria-valuenow={Math.round(currentTime)}
        aria-valuetext={`${fmtSecs(currentTime)} of ${fmtSecs(duration)}`}
        aria-disabled={scrubDisabled}
        onPointerDown={onTrackPointerDown}
        onPointerMove={onTrackPointerMove}
        onPointerUp={onTrackPointerUp}
        onPointerLeave={onTrackPointerLeave}
        onKeyDown={onTrackKeyDown}
        className={
          "group relative flex h-6 min-w-0 flex-1 cursor-pointer touch-none items-center " +
          (scrubDisabled ? "cursor-not-allowed opacity-50" : "") +
          " focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:ring-offset-0 rounded-full"
        }
      >
        {/* Track + fills. Track thickens on hover; thumb appears/enlarges. */}
        <div className="relative h-1.5 w-full overflow-visible rounded-full bg-muted transition-[height] duration-150 group-hover:h-2 motion-reduce:transition-none">
          {/* Buffered ranges (subtle; ~full for local files) */}
          <div
            className="absolute left-0 top-0 h-full rounded-full bg-muted-foreground/25"
            style={{ width: `${bufferedPct}%` }}
          />
          {/* Progress fill (ref-driven: rAF while playing, pointer while dragging) */}
          <div ref={fillRef} className="absolute left-0 top-0 h-full rounded-full bg-primary" />
          {/* Thumb (ref-driven left; appears/enlarges on hover or drag). The
              drag-enlarge is driven by `dragging` state (not a data-attribute
              off the ref) since dragging starts without a re-render that could
              update an attribute. */}
          <div
            ref={thumbRef}
            className={
              "absolute top-1/2 size-3 -translate-x-1/2 -translate-y-1/2 rounded-full bg-primary shadow-sm ring-ring/50 opacity-0 transition-[width,height,opacity] duration-150 group-hover:size-4 group-hover:opacity-100 motion-reduce:transition-none " +
              (dragging ? "size-4 opacity-100" : "")
            }
          />
        </div>

        {/* Hover / scrub time preview */}
        {previewSecs != null && previewPct != null && (
          <div
            className="pointer-events-none absolute bottom-full mb-2 -translate-x-1/2 whitespace-nowrap rounded-md bg-foreground px-2 py-0.5 text-xs text-background"
            style={{ left: `${previewPct}%` }}
          >
            {fmtSecs(previewSecs)}
          </div>
        )}
      </div>

      {/* Zone 3 — time readout + speed */}
      <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
        {fmtSecs(currentTime)} / {fmtSecs(duration)}
      </span>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            aria-label="Playback speed"
            className="shrink-0 gap-1 px-2 text-xs"
          >
            <Gauge className="size-4" />
            {rate}×
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="top">
          <DropdownMenuRadioGroup value={String(rate)} onValueChange={(v) => changeRate(Number(v))}>
            {RATES.map((r) => (
              <DropdownMenuRadioItem key={r} value={String(r)}>
                {r}×
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>

      {/* Hidden audio elements. The leader's timeupdate drives the readout. */}
      <audio
        ref={micRef}
        src={micSrc}
        preload="metadata"
        onLoadedMetadata={onLoadedMetadata}
        onProgress={() => {
          const el = leaderEl();
          if (!el || duration <= 0) return;
          const buf = el.buffered;
          setBufferedPct(buf.length ? (buf.end(buf.length - 1) / duration) * 100 : 0);
        }}
        onTimeUpdate={() => {
          if (leader !== "mic") return;
          const t = micRef.current?.currentTime ?? 0;
          setCurrentTime(t);
          onTimeUpdate?.(t * 1000);
        }}
        onPlay={() => {
          setIsPlaying(true);
          setWaiting(false);
        }}
        onPause={() => {
          // Only flip to "paused" when BOTH are paused (they start together).
          if (systemRef.current?.paused) setIsPlaying(false);
        }}
        onEnded={() => {
          if (leader === "mic") {
            setIsPlaying(false);
            setCurrentTime(duration);
          }
        }}
        onWaiting={() => setWaiting(true)}
        onCanPlay={() => setWaiting(false)}
      />
      <audio
        ref={systemRef}
        src={systemSrc}
        preload="metadata"
        onLoadedMetadata={onLoadedMetadata}
        onProgress={() => {
          const el = leaderEl();
          if (!el || duration <= 0) return;
          const buf = el.buffered;
          setBufferedPct(buf.length ? (buf.end(buf.length - 1) / duration) * 100 : 0);
        }}
        onTimeUpdate={() => {
          if (leader !== "system") return;
          const t = systemRef.current?.currentTime ?? 0;
          setCurrentTime(t);
          onTimeUpdate?.(t * 1000);
        }}
        onPlay={() => {
          setIsPlaying(true);
          setWaiting(false);
        }}
        onPause={() => {
          if (micRef.current?.paused) setIsPlaying(false);
        }}
        onEnded={() => {
          if (leader === "system") {
            setIsPlaying(false);
            setCurrentTime(duration);
          }
        }}
        onWaiting={() => setWaiting(true)}
        onCanPlay={() => setWaiting(false)}
      />
    </div>
  );
}
