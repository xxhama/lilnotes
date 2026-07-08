import { useEffect, useRef } from "react";

import { cn } from "@/lib/utils";
import type { TranscriptSegment } from "@/lib/ipc";

interface Props {
  segments: TranscriptSegment[];
  /** Auto-scroll to the newest segment (live mode). */
  follow?: boolean;
  className?: string;
}

function fmtTime(ms: number): string {
  const s = Math.floor(ms / 1000);
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

/** Speaker chip: "Me" for the mic channel, "Speaker" for system (diarized
 *  per-speaker labels arrive in milestone 4). */
function SpeakerChip({ source }: { source: TranscriptSegment["source"] }) {
  const me = source === "mic";
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
        me
          ? "bg-primary/10 text-primary"
          : "bg-secondary text-muted-foreground",
      )}
    >
      {me ? "Me" : "Speaker"}
    </span>
  );
}

export default function TranscriptPane({ segments, follow, className }: Props) {
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (follow) endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [segments.length, follow]);

  if (segments.length === 0) {
    return (
      <div className={cn("flex items-center justify-center p-6", className)}>
        <p className="text-xs text-muted-foreground">
          Transcript will appear here as speech is recognized.
        </p>
      </div>
    );
  }

  const sorted = [...segments].sort((a, b) => a.startMs - b.startMs);

  return (
    <div className={cn("space-y-2.5 overflow-y-auto p-4", className)} data-selectable>
      {sorted.map((s, i) => (
        <div key={`${s.source}-${s.startMs}-${i}`} className="flex items-start gap-2.5">
          <span className="w-9 shrink-0 pt-0.5 text-right font-mono text-[11px] tabular-nums text-muted-foreground/70">
            {fmtTime(s.startMs)}
          </span>
          <SpeakerChip source={s.source} />
          <p className="min-w-0 text-sm leading-relaxed">{s.text}</p>
        </div>
      ))}
      <div ref={endRef} />
    </div>
  );
}
