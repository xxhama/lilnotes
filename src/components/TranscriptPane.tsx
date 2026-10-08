import { Fragment, useEffect, useRef, useState } from "react";

import PickerCombobox, { type PickerItem } from "@/components/PickerCombobox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { Persona, SpeakerCandidates, SpeakerLink, TranscriptSegment } from "@/lib/ipc";

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
  /** Voice matches + customer roster that rank the picker by relevance.
   *  Omitted = a single alphabetical list. */
  candidates?: SpeakerCandidates;
  onConfirmPersona?: (raw: string, personaId: number) => void;
  onUnlinkPersona?: (raw: string) => void;
  onCreatePersona?: (name: string) => Promise<number>;
  /** When provided, each group's timestamp becomes a button that seeks the
   *  audio player to that timestamp. Omitted on the live Recording page. */
  onSeek?: (startMs: number) => void;
  /** Leader playback position (ms). The last group whose start ≤ this is
   * highlighted and scrolled into view while audio plays. */
  currentMs?: number;
  /** Mark every segment id in a Me group as echo (hides it; retained for
   *  offline echo re-processing). Only offered on mic ("Me") groups. */
  onMarkEcho?: (ids: number[]) => Promise<void> | void;
  /** Run offline echo cancellation on a single mic group's time range
   *  `[startMs, endMs]` (learns from marked echo regions; applies only there).
   *  Only offered on mic ("Me") speech groups. */
  onCleanEchoRange?: (startMs: number, endMs: number) => Promise<void> | void;
  /** Soft-delete every segment id in a Me group. */
  onDeleteSegment?: (ids: number[]) => Promise<void> | void;
  /** Revert an echo mark on a group (shown in "show hidden" mode). */
  onUnmarkEcho?: (ids: number[]) => Promise<void> | void;
  /** Restore a soft-deleted group (shown in "show hidden" mode). */
  onRestoreSegment?: (ids: number[]) => Promise<void> | void;
  /** When true, `segments` includes echo-marked + soft-deleted rows and they
   *  render dimmed with Unmark/Restore actions. When false (default), the
   *  caller passes only visible segments. */
  showHidden?: boolean;
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

/** Picker rows for one speaker, most relevant first: personas whose voice
 * matches this label ("Voice match", best score first), then people seen in
 * this meeting's customer's meetings ("From <customer>", most meetings
 * first), then everyone else alphabetically. Each persona appears once, in
 * its highest section; the current link is excluded (shown as "Current"). */
function orderPersonaItems(
  personas: Persona[],
  raw: string,
  currentId: number | null,
  candidates: SpeakerCandidates | undefined,
): PickerItem[] {
  const placed = new Set<number>(currentId != null ? [currentId] : []);
  // Names come from the live persona list: candidates can lag a rename or
  // delete, and a persona that no longer exists is skipped.
  const byId = new Map(personas.map((p) => [p.id, p]));
  const items: PickerItem[] = [];
  const add = (id: number, sublabel: string, group: string) => {
    const p = byId.get(id);
    if (!p || placed.has(id)) return;
    placed.add(id);
    items.push({ id, label: p.displayName, sublabel, group });
  };
  for (const s of candidates?.voiceMatches[raw] ?? []) {
    add(s.personaId, `${Math.round(s.score * 100)}%`, "Voice match");
  }
  if (candidates?.customerName) {
    const group = `From ${candidates.customerName}`;
    for (const r of candidates.customerRoster) {
      add(r.personaId, `${r.meetingCount} mtgs`, group);
    }
  }
  for (const p of personas) {
    add(p.id, `${p.voiceprintCount} prints`, "All personas");
  }
  return items;
}

function SpeakerChip({
  segment,
  renames,
  onRename,
  speakerLinks,
  personas,
  candidates,
  onConfirmPersona,
  onUnlinkPersona,
  onCreatePersona,
}: {
  segment: TranscriptSegment;
  renames: Record<string, string>;
  onRename?: (raw: string, name: string) => void;
  speakerLinks?: Record<string, SpeakerLink>;
  personas?: Persona[];
  candidates?: SpeakerCandidates;
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
      <Input
        ref={inputRef}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
          if (e.key === "Escape") setEditing(false);
        }}
        placeholder={display}
        className="h-auto w-24 shrink-0 rounded-full bg-background px-2 py-0.5 text-[11px] font-medium"
      />
    );
  }

  const clickable = personaPickerEnabled || Boolean(onRename);

  const chipClass = cn(
    "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] leading-none font-medium",
    chipColor(raw, link?.personaId ?? null),
    link && !link.confirmed && "border border-dashed border-amber-500/50",
    clickable && "cursor-pointer hover:ring-1 hover:ring-ring/40",
  );

  // Plain span (not <sup>) so the confidence % centers on the name's baseline
  // via the button's flex `items-center`, instead of <sup>'s vertical-align:
  // super pinning it to the top of the line box.
  const chipContent = (
    <>
      {display}
      {link && !link.confirmed && link.confidence != null && (
        <span className="ml-1 text-[9px] font-normal text-amber-600/80">
          {Math.round(link.confidence * 100)}%
        </span>
      )}
    </>
  );

  if (personaPickerEnabled) {
    return (
      <PickerCombobox
        open={pickerOpen}
        onOpenChange={setPickerOpen}
        trigger={
          <button type="button" disabled={!clickable} className={chipClass}>
            {chipContent}
          </button>
        }
        items={orderPersonaItems(personas ?? [], raw, link?.personaId ?? null, candidates)}
        currentId={link?.personaId ?? null}
        currentLabel={link?.personaName ?? undefined}
        currentSublabel={
          link?.confidence != null ? `${Math.round(link.confidence * 100)}%` : undefined
        }
        onPick={(pid) => onConfirmPersona?.(raw, pid)}
        onCreate={onCreatePersona!}
        onUnassign={link && onUnlinkPersona ? () => onUnlinkPersona?.(raw) : undefined}
        unassignLabel="Unlink persona"
        createNoun="persona"
        placeholder="Search personas…"
      />
    );
  }

  return (
    <button
      type="button"
      onClick={() => {
        if (!clickable) return;
        setDraft(renames[raw] ?? "");
        setEditing(true);
      }}
      disabled={!clickable}
      className={chipClass}
    >
      {chipContent}
    </button>
  );
}

/** Per-group ⋯ menu for mic ("Me") rows: mark-as-echo / delete, and (in
 * "show hidden" mode) unmark / restore. `alwaysVisible` keeps the trigger
 * shown for hidden rows the user is actively managing. */
function GroupMenu({
  ids,
  isEcho,
  isDeleted,
  isMic,
  startMs,
  endMs,
  alwaysVisible,
  onMarkEcho,
  onCleanEchoRange,
  onDeleteSegment,
  onUnmarkEcho,
  onRestoreSegment,
}: {
  ids: number[];
  isEcho: boolean;
  isDeleted: boolean;
  isMic: boolean;
  startMs: number;
  endMs: number;
  alwaysVisible: boolean;
  onMarkEcho?: (ids: number[]) => Promise<void> | void;
  onCleanEchoRange?: (startMs: number, endMs: number) => Promise<void> | void;
  onDeleteSegment?: (ids: number[]) => Promise<void> | void;
  onUnmarkEcho?: (ids: number[]) => Promise<void> | void;
  onRestoreSegment?: (ids: number[]) => Promise<void> | void;
}) {
  // Live/ephemeral segments (id 0) aren't persisted — no mutations on them.
  const valid = ids.filter((id) => id > 0);
  if (valid.length === 0) return null;
  // Only render if at least one action applies to this group's state.
  const hasMark = !isEcho && !isDeleted && onMarkEcho;
  // Per-segment echo clean: mic speech groups only (the user's own voice is
  // what gets de-echoed; system/speaker groups are never touched).
  const hasCleanEcho = isMic && !isEcho && !isDeleted && onCleanEchoRange;
  const hasDelete = !isDeleted && onDeleteSegment;
  const hasUnmark = isEcho && onUnmarkEcho;
  const hasRestore = isDeleted && onRestoreSegment;
  if (!hasMark && !hasCleanEcho && !hasDelete && !hasUnmark && !hasRestore) return null;

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label="Segment actions"
          className={cn(
            "inline-flex h-5 w-5 shrink-0 items-center justify-center rounded text-muted-foreground transition-opacity hover:bg-accent hover:text-foreground",
            alwaysVisible
              ? "opacity-70"
              : "opacity-0 peer-hover:opacity-100 hover:opacity-100 focus-within:opacity-100",
          )}
        >
          <span aria-hidden className="text-sm leading-none">
            ⋯
          </span>
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-44">
        {hasUnmark && (
          <DropdownMenuItem onSelect={() => onUnmarkEcho?.(valid)}>Unmark echo</DropdownMenuItem>
        )}
        {hasRestore && (
          <DropdownMenuItem onSelect={() => onRestoreSegment?.(valid)}>
            Restore segment
          </DropdownMenuItem>
        )}
        {hasMark && (
          <DropdownMenuItem onSelect={() => onMarkEcho?.(valid)}>Mark as echo</DropdownMenuItem>
        )}
        {hasCleanEcho && (
          <DropdownMenuItem onSelect={() => onCleanEchoRange?.(startMs, endMs)}>
            Clean echo in this segment
          </DropdownMenuItem>
        )}
        {hasDelete && (
          <DropdownMenuItem onSelect={() => onDeleteSegment?.(valid)}>
            Delete segment
          </DropdownMenuItem>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
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
  candidates,
  onConfirmPersona,
  onUnlinkPersona,
  onCreatePersona,
  onSeek,
  currentMs,
  onMarkEcho,
  onCleanEchoRange,
  onDeleteSegment,
  onUnmarkEcho,
  onRestoreSegment,
  showHidden = false,
}: Props) {
  const endRef = useRef<HTMLDivElement>(null);
  const activeRef = useRef<HTMLParagraphElement>(null);

  useEffect(() => {
    if (follow) endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [segments.length, follow]);

  const sorted = [...segments].sort((a, b) => a.startMs - b.startMs);

  // Collapse runs of consecutive same-speaker segments into one block so a
  // long monologue shows a single chip instead of repeating it per line.
  // Grouping identity: "Me" for the mic channel; otherwise the raw speaker
  // label — *unless* the label has a confirmed persona link, in which case
  // group by the persona id. That way two labels the model split (e.g.
  // SPEAKER_00 + SPEAKER_01) that the user confirmed as the same person
  // merge into one block (one name + color, adjacent runs joined). Only
  // *adjacent* segments merge — a speaker going silent and resuming later
  // starts a fresh group. Tagging keys off the identity, not per-segment,
  // so one chip per run is sufficient (confirm/unlink/rename apply to all).
  // Unconfirmed persona suggestions keep their raw-label group (matches
  // count_speaker_identities). Empty key (live system segments before
  // diarization) is never merged: each row stands alone so distinct remote
  // speakers don't collapse into one block while the mic is silent.
  const groups: { speakerKey: string; segments: TranscriptSegment[] }[] = [];
  for (const s of sorted) {
    let key = s.source === "mic" ? "Me" : (s.speaker ?? "");
    if (s.source !== "mic" && s.speaker) {
      const link = speakerLinks?.[s.speaker];
      if (link?.confirmed && link.personaId != null) key = `p:${link.personaId}`;
    }
    // In "show hidden" mode, keep echo / deleted rows in their own groups so
    // they don't merge into an adjacent normal run (they render + act
    // differently). The DB already filters them out when showHidden is false.
    if (showHidden) {
      const tag = s.kind === "echo" ? "echo" : s.deleted ? "deleted" : "normal";
      key = `${key}|${tag}`;
    }
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

  // One CSS grid for the whole transcript:
  // `[time] auto [chip] 1fr [text] auto [actions]`. The chip column auto-sizes
  // to the widest chip across all groups, so the text column starts at a
  // uniform x for every row. Each group is a single row — the group's start
  // timestamp + chip + the run's texts joined into one flowing paragraph (ASR
  // often emits one-word segments; joining them makes a speaker run read as
  // coherent prose instead of a word column). The trailing actions column
  // holds the per-row ⋯ menu (mark-as-echo / delete), revealed on hover for
  // normal rows and always shown for hidden rows the user is managing.
  return (
    <ScrollArea className={className}>
      <div
        className="grid grid-cols-[2.25rem_auto_1fr_auto] items-baseline gap-x-2.5 gap-y-2 p-4"
        data-selectable
      >
        {groups.map((group, gi) => {
          const first = group.segments[0];
          const isEcho = first.kind === "echo";
          const isDeleted = first.deleted === true;
          const isHiddenRow = showHidden && (isEcho || isDeleted);
          const dim = isHiddenRow ? "text-muted-foreground/60" : "";
          // `white-space: normal` (the <p> default) collapses stray double
          // spaces from empty / trailing-whitespace chunks, so a plain join
          // is enough — no extra trimming needed.
          const text = group.segments.map((s) => s.text).join(" ");
          return (
            <Fragment key={`g-${gi}`}>
              {onSeek ? (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <button
                      onClick={() => onSeek(first.startMs)}
                      className={cn(
                        "w-full shrink-0 text-right font-mono text-[11px] tabular-nums leading-none transition-colors hover:text-foreground",
                        dim,
                        activeGroupIndex === gi ? "text-foreground" : "text-muted-foreground/70",
                      )}
                    >
                      {fmtTime(first.startMs)}
                    </button>
                  </TooltipTrigger>
                  <TooltipContent>{`Play from ${fmtTime(first.startMs)}`}</TooltipContent>
                </Tooltip>
              ) : (
                <span
                  className={cn(
                    "text-right font-mono text-[11px] tabular-nums leading-none",
                    dim,
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
                  candidates={candidates}
                  onConfirmPersona={onConfirmPersona}
                  onUnlinkPersona={onUnlinkPersona}
                  onCreatePersona={onCreatePersona}
                />
              </div>
              <p
                ref={activeGroupIndex === gi ? activeRef : undefined}
                className={cn(
                  // `peer` so the actions cell can reveal on hover of the text.
                  // Every row carries the same padding so activating a row
                  // (toggling bg + border) never changes its size — the page
                  // can't jump when the highlight moves. `-mx-2` cancels the
                  // horizontal padding so text stays aligned with the chip and
                  // timestamp columns; `items-baseline` keeps the three aligned
                  // vertically despite the row padding.
                  "peer min-w-0 -mx-2 rounded-sm px-2 py-1 text-sm leading-relaxed transition-colors",
                  onSeek && "cursor-pointer",
                  activeGroupIndex === gi && "bg-accent/70 transcript-row-active",
                  isHiddenRow && "text-muted-foreground/60 line-through",
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
              <div className="flex items-center justify-end gap-1">
                <GroupMenu
                  ids={group.segments.map((s) => s.id)}
                  isEcho={isEcho}
                  isDeleted={isDeleted}
                  isMic={first.source === "mic"}
                  startMs={first.startMs}
                  endMs={group.segments[group.segments.length - 1]?.endMs ?? first.startMs}
                  alwaysVisible={isHiddenRow}
                  onMarkEcho={onMarkEcho}
                  onCleanEchoRange={onCleanEchoRange}
                  onDeleteSegment={onDeleteSegment}
                  onUnmarkEcho={onUnmarkEcho}
                  onRestoreSegment={onRestoreSegment}
                />
              </div>
            </Fragment>
          );
        })}
        <div ref={endRef} className="col-span-4" />
      </div>
    </ScrollArea>
  );
}
