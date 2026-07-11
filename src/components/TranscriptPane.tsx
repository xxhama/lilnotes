import { Fragment, useEffect, useRef, useState } from "react";

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
  /** When provided, each group's timestamp becomes a button that seeks the
   *  audio player to that timestamp. Omitted on the live Recording page. */
  onSeek?: (startMs: number) => void;
  /** Leader playback position (ms). The last group whose start ≤ this is
   * highlighted and scrolled into view while audio plays. */
  currentMs?: number;
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

function chipColor(raw: string, personaId: number | null = null): string {
  // When a persona is linked, color by personaId so every raw label
  // confirmed/linked to the same persona shares one color.
  if (personaId != null) {
    return CHIP_COLORS[personaId % CHIP_COLORS.length];
  }
  // Diarized labels ("SPEAKER_00") color by their index digits.
  const digits = parseInt(raw.replace(/\D/g, ""), 10);
  if (!Number.isNaN(digits)) {
    return CHIP_COLORS[digits % CHIP_COLORS.length];
  }
  // No digits (e.g. a live-identified persona name) — hash the string so
  // different names get different colors.
  let hash = 0;
  for (let i = 0; i < raw.length; i++) {
    hash = ((hash << 5) - hash + raw.charCodeAt(i)) | 0;
  }
  return CHIP_COLORS[Math.abs(hash) % CHIP_COLORS.length];
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
      <span className="inline-flex shrink-0 items-center rounded-full bg-primary/10 px-2 py-0.5 text-[11px] leading-none font-medium text-primary">
        Me
      </span>
    );
  }
  if (!raw) {
    return (
      <span className="inline-flex shrink-0 items-center rounded-full bg-secondary px-2 py-0.5 text-[11px] leading-none font-medium text-muted-foreground">
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
          "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] leading-none font-medium",
          chipColor(raw, link?.personaId ?? null),
          link && !link.confirmed && "border border-dashed border-amber-500/50",
          clickable && "cursor-pointer hover:ring-1 hover:ring-ring/40",
        )}
      >
        {display}
        {link && !link.confirmed && link.confidence != null && (
          // Plain span (not <sup>) so the percentage centers on the
          // name's baseline via the flex `items-center` on the button,
          // instead of <sup>'s default vertical-align: super pinning it
          // to the top of the line box.
          <span className="ml-1 text-[9px] font-normal text-amber-600/80">
            {Math.round(link.confidence * 100)}%
          </span>
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
  onSeek,
  currentMs,
}: Props) {
  const endRef = useRef<HTMLDivElement>(null);
  const activeRef = useRef<HTMLParagraphElement>(null);

  useEffect(() => {
    if (follow) endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [segments.length, follow]);

  const sorted = [...segments].sort((a, b) => a.startMs - b.startMs);

  // Collapse runs of consecutive same-speaker segments into one block so a
  // long monologue shows a single chip instead of repeating it per line.
  // Grouping identity: "Me" for the mic channel, otherwise the raw speaker
  // label. Only *adjacent* segments merge — a speaker going silent and
  // resuming later starts a fresh group. Tagging keys off the raw label, not
  // per-segment, so one chip per run is sufficient (confirm/unlink/rename
  // apply to all). Empty key (live system segments before diarization) is
  // never merged: each row stands alone so distinct remote speakers don't
  // collapse into one block while the mic is silent.
  const groups: { speakerKey: string; segments: TranscriptSegment[] }[] = [];
  for (const s of sorted) {
    const key = s.source === "mic" ? "Me" : (s.speaker ?? "");
    const last = groups[groups.length - 1];
    if (last && last.speakerKey === key && key !== "") last.segments.push(s);
    else groups.push({ speakerKey: key, segments: [s] });
  }

  // Active group = the last one whose start is at/before the playhead. Null
  // when nothing is playing. Drives the highlight + auto-scroll below.
  let activeGroupIndex: number | null = null;
  if (currentMs != null) {
    for (let i = 0; i < groups.length; i++) {
      if (groups[i].segments[0].startMs <= currentMs) activeGroupIndex = i;
      else break;
    }
  }

  useEffect(() => {
    if (activeGroupIndex == null) return;
    // `nearest` only scrolls when the row is off-screen, so this never
    // fights the user's manual scroll.
    activeRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [activeGroupIndex]);

  if (segments.length === 0) {
    return (
      <div className={cn("flex items-center justify-center p-6", className)}>
        <p className="text-xs text-muted-foreground">
          Transcript will appear here as speech is recognized.
        </p>
      </div>
    );
  }

  // One CSS grid for the whole transcript: `[time] auto [chip] 1fr [text]`.
  // The chip column auto-sizes to the widest chip across all groups, so the
  // text column starts at a uniform x for every row. Each group is a single
  // row — the group's start timestamp + chip + the run's texts joined into
  // one flowing paragraph (ASR often emits one-word segments; joining them
  // makes a speaker run read as coherent prose instead of a word column).
  return (
    <div className={cn("flex h-full flex-col overflow-y-auto", className)}>
      <div
        className="grid grid-cols-[2.25rem_auto_1fr] items-baseline gap-x-2.5 gap-y-2 p-4"
        data-selectable
      >
        {groups.map((group, gi) => {
          const first = group.segments[0];
          // `white-space: normal` (the <p> default) collapses stray double
          // spaces from empty / trailing-whitespace chunks, so a plain join
          // is enough — no extra trimming needed.
          const text = group.segments.map((s) => s.text).join(" ");
          return (
            <Fragment key={`g-${gi}`}>
              {onSeek ? (
                <button
                  onClick={() => onSeek(first.startMs)}
                  title={`Play from ${fmtTime(first.startMs)}`}
                  className={cn(
                    "w-full shrink-0 text-right font-mono text-[11px] tabular-nums leading-none transition-colors hover:text-foreground",
                    activeGroupIndex === gi ? "text-foreground" : "text-muted-foreground/70",
                  )}
                >
                  {fmtTime(first.startMs)}
                </button>
              ) : (
                <span
                  className={cn(
                    "text-right font-mono text-[11px] tabular-nums leading-none",
                    activeGroupIndex === gi ? "text-foreground" : "text-muted-foreground/70",
                  )}
                >
                  {fmtTime(first.startMs)}
                </span>
              )}
              <div>
                <SpeakerChip
                  segment={first}
                  renames={renames}
                  onRename={onRenameSpeaker}
                  speakerLinks={speakerLinks}
                  personas={personas}
                  onConfirmPersona={onConfirmPersona}
                  onUnlinkPersona={onUnlinkPersona}
                  onCreatePersona={onCreatePersona}
                />
              </div>
              <p
                ref={activeGroupIndex === gi ? activeRef : undefined}
                className={cn(
                  // Every row carries the same padding so activating a row
                  // (toggling bg + border) never changes its size — the page
                  // can't jump when the highlight moves. `-mx-2` cancels the
                  // horizontal padding so text stays aligned with the chip and
                  // timestamp columns; `items-baseline` keeps the three aligned
                  // vertically despite the row padding.
                  "min-w-0 -mx-2 rounded-sm px-2 py-1 text-sm leading-relaxed transition-colors",
                  onSeek && "cursor-pointer",
                  activeGroupIndex === gi && "bg-accent/70 transcript-row-active",
                )}
                onClick={
                  onSeek
                    ? () => {
                        // Skip when the user was drag-selecting text — a plain
                        // click collapses the selection, so a non-empty selection
                        // here means they meant to select, not to seek.
                        if (window.getSelection()?.toString()) return;
                        onSeek(first.startMs);
                      }
                    : undefined
                }
              >
                {text}
              </p>
            </Fragment>
          );
        })}
        <div ref={endRef} className="col-span-3" />
      </div>
    </div>
  );
}
