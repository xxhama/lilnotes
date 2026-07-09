import { useEffect, useRef, useState } from "react";

import SpeakerPersonaPicker from "@/components/SpeakerPersonaPicker";
import { cn } from "@/lib/utils";
import type { Persona, SpeakerLink, TranscriptSegment } from "@/lib/ipc";

interface Props {
  segments: TranscriptSegment[];
  /** Auto-scroll to the newest segment (live mode). */
  follow?: boolean;
  /** Display-name overrides for raw speaker labels (SPEAKER_00 → "Priya"). */
  renames?: Record<string, string>;
  /** Called with the raw label + new name when the user renames a speaker. */
  onRenameSpeaker?: (raw: string, name: string) => void;
  className?: string;
  /** raw label -> persona link (suggestion/confirmed) per meeting. */
  speakerLinks?: Record<string, SpeakerLink>;
  /** All personas for the picker's "choose different" list. */
  personas?: Persona[];
  onConfirmPersona?: (raw: string, personaId: number) => void;
  onUnlinkPersona?: (raw: string) => void;
  onCreatePersona?: (name: string) => Promise<number>;
}

function fmtTime(ms: number): string {
  const s = Math.floor(ms / 1000);
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

/** Stable-ish color per speaker for quick visual scanning. */
const CHIP_COLORS = [
  "bg-blue-500/10 text-blue-700 dark:text-blue-400",
  "bg-amber-500/10 text-amber-700 dark:text-amber-400",
  "bg-purple-500/10 text-purple-700 dark:text-purple-400",
  "bg-teal-500/10 text-teal-700 dark:text-teal-400",
  "bg-rose-500/10 text-rose-700 dark:text-rose-400",
  "bg-lime-600/10 text-lime-700 dark:text-lime-400",
];

function chipColor(raw: string): string {
  const n = parseInt(raw.replace(/\D/g, ""), 10);
  return CHIP_COLORS[(Number.isNaN(n) ? 0 : n) % CHIP_COLORS.length];
}

function SpeakerChip({
  segment,
  renames,
  onRename,
  speakerLinks,
  personas,
  onConfirmPersona,
  onUnlinkPersona,
  onCreatePersona,
}: {
  segment: TranscriptSegment;
  renames: Record<string, string>;
  onRename?: (raw: string, name: string) => void;
  speakerLinks?: Record<string, SpeakerLink>;
  personas?: Persona[];
  onConfirmPersona?: (raw: string, personaId: number) => void;
  onUnlinkPersona?: (raw: string) => void;
  onCreatePersona?: (name: string) => Promise<number>;
}) {
  const raw = segment.speaker;
  const [pickerOpen, setPickerOpen] = useState(false);

  // Legacy free-text fallback (used only when onConfirmPersona is not wired).
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (editing) inputRef.current?.focus();
  }, [editing]);

  if (segment.source === "mic") {
    return (
      <span className="inline-flex shrink-0 items-center rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-medium text-primary">
        Me
      </span>
    );
  }
  if (!raw) {
    return (
      <span className="inline-flex shrink-0 items-center rounded-full bg-secondary px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
        Speaker
      </span>
    );
  }

  const link = speakerLinks?.[raw];
  const display = link?.personaName ?? renames[raw] ?? raw;
  const personaPickerEnabled = Boolean(onConfirmPersona && onCreatePersona);

  // Legacy free-text commit (fallback path only).
  const commit = () => {
    setEditing(false);
    const name = draft.trim();
    if (name && onRename) onRename(raw, name);
  };

  if (editing && !personaPickerEnabled) {
    return (
      <input
        ref={inputRef}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
          if (e.key === "Escape") setEditing(false);
        }}
        placeholder={display}
        className="w-24 shrink-0 rounded-full border bg-background px-2 py-0.5 text-[11px] font-medium outline-none focus:border-ring"
      />
    );
  }

  const clickable = personaPickerEnabled || Boolean(onRename);

  return (
    <span className="relative inline-flex shrink-0">
      <button
        onClick={() => {
          if (!clickable) return;
          if (personaPickerEnabled) {
            setPickerOpen((v) => !v);
          } else {
            setDraft(renames[raw] ?? "");
            setEditing(true);
          }
        }}
        disabled={!clickable}
        title={clickable ? `Assign ${display}` : undefined}
        className={cn(
          "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
          chipColor(raw),
          link && !link.confirmed && "border border-dashed border-amber-500/50",
          clickable && "cursor-pointer hover:ring-1 hover:ring-ring/40",
        )}
      >
        {display}
        {link && !link.confirmed && link.confidence != null && (
          <sup className="ml-0.5 text-[9px] font-normal text-amber-600/80">
            {Math.round(link.confidence * 100)}%
          </sup>
        )}
      </button>
      {pickerOpen && personaPickerEnabled && (
        <SpeakerPersonaPicker
          rawLabel={raw}
          current={
            link?.personaId != null && personas
              ? {
                  personaId: link.personaId,
                  displayName: link.personaName ?? raw,
                  score: link.confidence ?? 0,
                  tier: link.confirmed ? "auto" : "suggest",
                }
              : null
          }
          personas={personas ?? []}
          onConfirm={(pid) => {
            onConfirmPersona?.(raw, pid);
            setPickerOpen(false);
          }}
          onCreatePersona={onCreatePersona!}
          onDismiss={() => setPickerOpen(false)}
          onUnlink={() => {
            onUnlinkPersona?.(raw);
            setPickerOpen(false);
          }}
        />
      )}
    </span>
  );
}

export default function TranscriptPane({
  segments,
  follow,
  renames = {},
  onRenameSpeaker,
  className,
  speakerLinks,
  personas,
  onConfirmPersona,
  onUnlinkPersona,
  onCreatePersona,
}: Props) {
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
          <SpeakerChip
            segment={s}
            renames={renames}
            onRename={onRenameSpeaker}
            speakerLinks={speakerLinks}
            personas={personas}
            onConfirmPersona={onConfirmPersona}
            onUnlinkPersona={onUnlinkPersona}
            onCreatePersona={onCreatePersona}
          />
          <p className="min-w-0 text-sm leading-relaxed">{s.text}</p>
        </div>
      ))}
      <div ref={endRef} />
    </div>
  );
}