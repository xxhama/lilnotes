/**
 * Transcript toolbar — the single row of controls between the audio player and
 * the transcript body on Meeting Detail.
 *
 * Three zones, progressive disclosure:
 *   1. Operations  — Re-transcribe, Re-identify speakers, Clean echo (badge),
 *      Revert echo clean. All secondary/ghost weight (no filled-primary in the
 *      bar); the transcript is the screen's primary content, not these buttons.
 *   2. Settings     — one quiet button opening a Popover holding the Model and
 *      Remote-speakers selects (config that's set once and rarely changed), with
 *      a dirty dot when a setting differs from its default. The popover's
 *      "Apply & re-transcribe" is the only filled-primary control.
 *   3. View toggle  — "N hidden" eye toggle on the far right, separated by flex
 *      space; a view filter, kept apart from the processing actions.
 *
 * Behaviors (verified against the backend, not guessed):
 *   - Model select applies only on Re-transcribe (doesn't change the global
 *     model). Helper text: "Used when re-transcribing."
 *   - Remote speakers applies on Re-identify speakers (ignored by re-transcribe,
 *     clean echo, revert). Helper text: "Used when re-identifying speakers."
 *   - Re-transcribe is destructive-ish → routes through the existing AlertDialog
 *     confirm (the app's only confirm pattern), both from the bar button and the
 *     popover's "Apply & re-transcribe".
 *   - Clean echo stays enabled at 0 marks (preserves unsupervised cleanup); the
 *     count badge shows only when hiddenSegmentCount > 0.
 *
 * Accessibility: `role="toolbar"` with roving tabindex — Tab enters at one
 * control, Arrow keys move between controls, Tab exits. Enter/Space opens the
 * Settings popover (Radix handles), Escape closes it and returns focus to the
 * trigger. aria-pressed on the view toggle; aria-label on the icon-only Revert.
 */
import { type KeyboardEvent, useLayoutEffect, useRef, useState } from "react";
import {
  Eye,
  EyeOff,
  Loader2,
  RefreshCw,
  RotateCcw,
  SlidersHorizontal,
  Sparkles,
  Users,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverDescription,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { AsrModelInfo } from "@/lib/ipc";

type Busy = "transcribing" | "diarizing" | "cleaning" | null;

interface TranscriptToolbarProps {
  hasAudio: boolean;
  busy: Busy;
  /** 0..=1 cleaning progress; only meaningful when `busy === "cleaning"`. */
  aecPct: number | null;
  needsDiarization: boolean;
  hiddenSegmentCount: number;
  hasCleanedMic: boolean;
  downloadedModels: AsrModelInfo[];
  retranscribeModel: string;
  onRetranscribeModelChange: (id: string) => void;
  /** The reset value for the Model select (meeting's recorded model, else the
   * global active, else first downloaded). Drives the Settings dirty dot. */
  defaultModelId: string;
  numSpeakers: string;
  onNumSpeakersChange: (v: string) => void;
  showHidden: boolean;
  onToggleHidden: () => void;
  /** Open the existing Re-transcribe confirm dialog. */
  onRetranscribe: () => void;
  onReidentify: () => void;
  onCleanEcho: () => void;
  onRevertEchoClean: () => void;
}

export default function TranscriptToolbar({
  hasAudio,
  busy,
  aecPct,
  needsDiarization,
  hiddenSegmentCount,
  hasCleanedMic,
  downloadedModels,
  retranscribeModel,
  onRetranscribeModelChange,
  defaultModelId,
  numSpeakers,
  onNumSpeakersChange,
  showHidden,
  onToggleHidden,
  onRetranscribe,
  onReidentify,
  onCleanEcho,
  onRevertEchoClean,
}: TranscriptToolbarProps) {
  const toolbarRef = useRef<HTMLDivElement>(null);
  const [activeIdx, setActiveIdx] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);

  // Declarative control count — drives the roving-tabindex clamp so one control
  // always has tabIndex=0 even as Revert / Show-hidden appear and disappear.
  const opsCount = hasAudio && busy === null ? 3 + (hasCleanedMic ? 1 : 0) : 0;
  const settingsCount = hasAudio ? 1 : 0;
  const toggleCount = hiddenSegmentCount > 0 ? 1 : 0;
  const controlCount = opsCount + settingsCount + toggleCount;

  useLayoutEffect(() => {
    if (controlCount > 0 && activeIdx >= controlCount) setActiveIdx(controlCount - 1);
  }, [controlCount, activeIdx]);

  // Roving tabindex: each focusable control gets a sequential slot. The counter
  // resets every render; JSX is evaluated in DOM order so indices line up with
  // querySelectorAll order in the arrow-key handler.
  let counter = 0;
  const slot = () => {
    const idx = counter++;
    return {
      tabIndex: idx === activeIdx ? 0 : -1,
      onFocus: () => setActiveIdx(idx),
    };
  };

  const onToolbarKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const root = toolbarRef.current;
    if (!root) return;
    // PopoverContent is portaled to <body>, so this only finds bar buttons.
    const focusables = Array.from(
      root.querySelectorAll<HTMLButtonElement>("button:not([disabled])"),
    );
    const count = focusables.length;
    if (count === 0) return;
    let next: number;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (activeIdx + 1) % count;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = (activeIdx - 1 + count) % count;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = count - 1;
    else return;
    e.preventDefault();
    setActiveIdx(next);
    focusables[next]?.focus();
  };

  const settingsDirty =
    (retranscribeModel !== "" && retranscribeModel !== defaultModelId) || numSpeakers !== "auto";

  const cleanEchoTooltip =
    hiddenSegmentCount > 0
      ? `${hiddenSegmentCount} hidden segment${hiddenSegmentCount === 1 ? "" : "s"} — re-process the mic to suppress echo, then re-transcribe.`
      : "Re-process the mic to suppress echo (unsupervised — no echo marks), then re-transcribe.";

  const canRetranscribe = downloadedModels.length > 0 && retranscribeModel !== "";

  return (
    <div
      ref={toolbarRef}
      role="toolbar"
      aria-label="Transcript tools"
      onKeyDown={onToolbarKeyDown}
      className="flex items-center gap-2 border-b px-4 py-2"
    >
      {/* Zone 1: operations (idle) or progress (busy) */}
      {hasAudio && busy !== null ? (
        <div className="flex items-center gap-3">
          {busy === "diarizing" && (
            <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Identifying speakers…
            </span>
          )}
          {busy === "transcribing" && (
            <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Re-transcribing…
            </span>
          )}
          {busy === "cleaning" && (
            <div className="flex flex-col gap-1">
              <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
                <Loader2 className="size-3.5 animate-spin" />
                {aecPct != null
                  ? `Cleaning echo… ${Math.round(aecPct * 100)}%`
                  : "Re-transcribing cleaned mic…"}
              </span>
              {aecPct != null && (
                <div className="h-1 w-48 overflow-hidden rounded-full bg-secondary">
                  <div
                    className="h-full bg-primary transition-[width] duration-150"
                    style={{ width: `${Math.round(aecPct * 100)}%` }}
                  />
                </div>
              )}
            </div>
          )}
        </div>
      ) : hasAudio && busy === null ? (
        <div className="flex items-center gap-2">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                size="sm"
                variant="secondary"
                onClick={onRetranscribe}
                disabled={!canRetranscribe}
                {...slot()}
              >
                <RefreshCw /> Re-transcribe
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              Re-transcribe with the selected model — does not change the model used for future
              recordings.
            </TooltipContent>
          </Tooltip>
          <Button size="sm" variant="secondary" onClick={onReidentify} {...slot()}>
            <Users /> {needsDiarization ? "Identify speakers" : "Re-identify speakers"}
          </Button>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button size="sm" variant="secondary" onClick={onCleanEcho} {...slot()}>
                <Sparkles /> Clean echo
                {hiddenSegmentCount > 0 && (
                  <span className="ml-1 inline-flex h-4 min-w-4 items-center justify-center rounded-full bg-primary/10 px-1 text-[10px] font-medium tabular-nums text-primary">
                    {hiddenSegmentCount}
                  </span>
                )}
              </Button>
            </TooltipTrigger>
            <TooltipContent>{cleanEchoTooltip}</TooltipContent>
          </Tooltip>
          {hasCleanedMic && (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  size="sm"
                  variant="ghost"
                  className="size-8 px-0"
                  onClick={onRevertEchoClean}
                  aria-label="Revert echo clean"
                  {...slot()}
                >
                  <RotateCcw />
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                Discard the echo-cleaned mic and re-transcribe from the original mic.wav.
              </TooltipContent>
            </Tooltip>
          )}
        </div>
      ) : null}

      {/* Divider between operations/settings (only when settings render) */}
      {hasAudio && <span className="h-5 w-px shrink-0 bg-border" aria-hidden />}

      {/* Zone 2: settings popover */}
      {hasAudio && (
        <Popover open={settingsOpen} onOpenChange={setSettingsOpen}>
          <PopoverTrigger asChild>
            <Button size="sm" variant="ghost" className="relative" {...slot()}>
              <SlidersHorizontal /> Settings
              {settingsDirty && (
                <span
                  className="absolute right-1 top-1 size-1.5 rounded-full bg-primary"
                  aria-hidden
                />
              )}
            </Button>
          </PopoverTrigger>
          <PopoverContent align="start" className="w-72">
            <PopoverHeader>
              <PopoverTitle>Transcription settings</PopoverTitle>
              <PopoverDescription>
                Changes apply on the next Re-transcribe or Re-identify.
              </PopoverDescription>
            </PopoverHeader>
            <div className="flex flex-col gap-3 pt-2">
              <div className="flex flex-col gap-1.5">
                <label htmlFor="tt-model" className="text-xs font-medium text-muted-foreground">
                  Model
                </label>
                <Select value={retranscribeModel} onValueChange={onRetranscribeModelChange}>
                  <SelectTrigger id="tt-model" className="text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent position="popper">
                    {downloadedModels.map((m) => (
                      <SelectItem key={m.id} value={m.id}>
                        {m.label}
                        {m.active ? " (live)" : ""}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-[11px] text-muted-foreground">Used when re-transcribing.</p>
              </div>
              <div className="flex flex-col gap-1.5">
                <label htmlFor="tt-remote" className="text-xs font-medium text-muted-foreground">
                  Remote speakers
                </label>
                <Select value={numSpeakers} onValueChange={onNumSpeakersChange}>
                  <SelectTrigger id="tt-remote" className="text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent position="popper">
                    <SelectItem value="auto">Auto</SelectItem>
                    {[1, 2, 3, 4, 5, 6, 7, 8].map((n) => (
                      <SelectItem key={n} value={String(n)}>
                        {n}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-[11px] text-muted-foreground">
                  Used when re-identifying speakers.
                </p>
              </div>
            </div>
            <div className="flex justify-end pt-3">
              <Button
                size="sm"
                onClick={() => {
                  setSettingsOpen(false);
                  onRetranscribe();
                }}
                disabled={!canRetranscribe || busy !== null}
              >
                <RefreshCw /> Apply &amp; re-transcribe
              </Button>
            </div>
          </PopoverContent>
        </Popover>
      )}

      {/* Zone 3: view toggle (far right, separated by flex space) */}
      {hiddenSegmentCount > 0 && (
        <>
          <div className="ml-auto" />
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                size="sm"
                variant={showHidden ? "secondary" : "ghost"}
                aria-pressed={showHidden}
                onClick={onToggleHidden}
                {...slot()}
              >
                {showHidden ? <EyeOff /> : <Eye />} {hiddenSegmentCount} hidden
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              {showHidden
                ? "Hide hidden segments"
                : `Show ${hiddenSegmentCount} hidden segment${hiddenSegmentCount === 1 ? "" : "s"}`}
            </TooltipContent>
          </Tooltip>
        </>
      )}
    </div>
  );
}
