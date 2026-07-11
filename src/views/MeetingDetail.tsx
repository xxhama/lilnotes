/**
 * Meeting detail view. Shows the full speaker-labeled transcript, audio
 * player (mic + system tracks), notes editor, summary panel with
 * streaming generation, speaker rename + persona assignment, and customer
 * linking. Fetches a meeting by ID via `getMeeting` and renders all
 * persisted data.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  Check,
  ChevronDown,
  FileText,
  Loader2,
  MoreHorizontal,
  Pencil,
  Trash2,
  Users,
} from "lucide-react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button, buttonVariants } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import AudioPlayer from "@/components/AudioPlayer";
import CustomerPicker from "@/components/CustomerPicker";
import NotesEditor from "@/components/NotesEditor";
import SummaryPanel from "@/components/SummaryPanel";
import TranscriptPane from "@/components/TranscriptPane";
import { cn } from "@/lib/utils";
import {
  confirmSpeakerPersona,
  createCustomer,
  createPersona,
  deleteMeeting,
  diarizeMeeting,
  getMeeting,
  listCustomers,
  listPersonas,
  onSpeakersIdentified,
  onVoiceprintsEnrolled,
  renameSpeaker,
  setMeetingCustomer,
  transcribeMeeting,
  unlinkSpeakerPersona,
  updateMeetingNotes,
  updateMeetingTitle,
  type CustomerSummary,
  type MeetingDetail as Meeting,
  type Persona,
} from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  meetingId: string;
  onNavigate: (route: Route) => void;
}

/** Minimum time the "Saving…" indicator stays visible so it doesn't flicker. */
const SAVE_MIN_VISIBLE_MS = 600;

function fmtDate(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
    year: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function fmtDuration(startMs: number, endMs: number | null): string {
  if (endMs == null) return "";
  const min = Math.round((endMs - startMs) / 60000);
  return min < 1 ? "<1 min" : min < 60 ? `${min} min` : `${Math.floor(min / 60)} h ${min % 60} min`;
}

/** Formats the notes last-saved timestamp for the "Saved" chip tooltip. */
function fmtSavedAt(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

/**
 * Meeting detail: editable title, persisted transcript with renamable
 * speakers. Summary panel arrives in M6; export in M7.
 */
export default function MeetingDetailView({ meetingId, onNavigate }: Props) {
  const id = Number(meetingId);
  const [meeting, setMeeting] = useState<Meeting | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"transcribing" | "diarizing" | null>(null);
  const [titleDraft, setTitleDraft] = useState<string | null>(null);
  /** "auto" or a declared remote-speaker count for re-identification. */
  const [numSpeakers, setNumSpeakers] = useState<string>("auto");
  const [personas, setPersonas] = useState<Persona[]>([]);
  const [customers, setCustomers] = useState<CustomerSummary[]>([]);
  const [assigning, setAssigning] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [tab, setTab] = useState<"review" | "transcript">("review");
  /** Seek request (ms from recording start) sent to the audio player when the
   * user clicks a transcript timestamp. A new value (even equal to the prior
   * one) re-triggers the seek; use a counter+ms pair so repeated clicks on the
   * same segment still replay. */
  const [seek, setSeek] = useState<{ ms: number; n: number } | null>(null);
  /** Leader playback position (ms) reported by the audio player so the
   * transcript can highlight + follow the active group. Null when idle. */
  const [currentMs, setCurrentMs] = useState<number | null>(null);
  /** Notes autosave status shown in the notes pane header. The resting state is
   * "saved" — the badge is always visible, not just after a modification. */
  const [notesStatus, setNotesStatus] = useState<"modified" | "saving" | "saved">("saved");
  /** Wall-clock time of the last successful persist (for the "Saved" tooltip). */
  const [savedAtMs, setSavedAtMs] = useState<number | null>(null);
  /** Delete-confirmation dialog (opened from the header overflow menu). */
  const [confirmDeleteOpen, setConfirmDeleteOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // Latest notes buffer + timers, kept in refs so the editor config (created
  // once) always persists the newest content without re-subscribing.
  const notesRef = useRef<string>("");
  const saveTimer = useRef<number | null>(null);
  const holdTimer = useRef<number | null>(null);
  // Bumped on every keystroke so a save completing for older content can't
  // clobber a newer "Modified" (or jump to "Saved" out of order).
  const saveGen = useRef(0);

  const reload = useCallback(() => {
    getMeeting(id)
      .then(setMeeting)
      .catch((e) => setError(String(e)));
  }, [id]);

  useEffect(reload, [reload]);

  // Seed the "Saved at" timestamp from the persisted value once the meeting for
  // the current id arrives (and again after any reload, e.g. diarization). The
  // editor isn't mounted until `meeting.id === id`, so no edits can race this.
  useEffect(() => {
    if (meeting && meeting.id === id) {
      setSavedAtMs(meeting.notesUpdatedAtMs);
    }
  }, [meeting, id]);

  // Reset the notes buffer + autosave state when the meeting changes (navigating
  // between meetings keeps the same component instance). Pending saves are
  // cancelled so stale content never overwrites the new meeting's notes.
  // `savedAtMs` is intentionally NOT reset here — the meeting-load effect seeds
  // it from the persisted value so the "Saved at" timestamp survives navigation.
  useEffect(() => {
    notesRef.current = "";
    setNotesStatus("saved");
    saveGen.current = 0;
    setSeek(null);
    setCurrentMs(null);
    if (saveTimer.current) {
      clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    if (holdTimer.current) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
  }, [id]);

  useEffect(() => {
    listPersonas()
      .then(setPersonas)
      .catch((e) => setError(String(e)));
    listCustomers()
      .then(setCustomers)
      .catch(() => {});
  }, []);

  const assignCustomer = useCallback(
    async (customerId: number | null) => {
      setAssigning(true);
      try {
        await setMeetingCustomer(id, customerId);
        reload();
      } catch (e) {
        setError(String(e));
      } finally {
        setAssigning(false);
      }
    },
    [id, reload],
  );

  const createCustomerInline = useCallback(async (name: string): Promise<number> => {
    const cid = await createCustomer(name);
    setCustomers(await listCustomers());
    return cid;
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onSpeakersIdentified(() => {
      if (!cancelled) reload();
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [reload]);

  // Refresh persona voiceprint counts when a background enrollment finishes.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onVoiceprintsEnrolled(() => {
      if (!cancelled)
        listPersonas()
          .then(setPersonas)
          .catch(() => {});
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const commitTitle = useCallback(async () => {
    if (!meeting || titleDraft === null) return;
    const title = titleDraft.trim();
    setTitleDraft(null);
    if (title && title !== meeting.title) {
      await updateMeetingTitle(id, title);
      reload();
    }
  }, [id, meeting, titleDraft, reload]);

  // Autosave flow: Modified (immediately on typing) → Saving… (1s after the
  // last keystroke) → Saved (after the write, held ~600ms so it doesn't flicker).
  //
  // Two callbacks back the editor:
  //  - onNotesActivity: fires on EVERY keystroke via our own ProseMirror plugin
  //    (Milkdown's markdownUpdated listener is debounced ~200ms, which would let
  //    the save timer fire mid-typing and flip the indicator out of order).
  //    Resets the debounce, clears any hold, and flips to "Modified" instantly.
  //  - onNotesMarkdown: fires from Milkdown's (debounced) markdownUpdated just to
  //    refresh the latest serialized markdown buffer used by the save.
  //
  // A generation counter guards against an in-flight save for older content
  // clobbering a newer "Modified" if the user resumes typing mid-save.
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
        await updateMeetingNotes(id, notesRef.current);
        // If the user typed again while we were saving, stay "Modified" and
        // let the newer debounce's save run — don't claim "Saved".
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
  }, [id]);

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

  const speakerCount = useCallback(
    () => (numSpeakers === "auto" ? undefined : Number(numSpeakers)),
    [numSpeakers],
  );

  const runTranscription = useCallback(async () => {
    setBusy("transcribing");
    setError(null);
    try {
      await transcribeMeeting(id);
      setBusy("diarizing");
      await diarizeMeeting(id, speakerCount());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      reload();
    }
  }, [id, reload, speakerCount]);

  const runDiarization = useCallback(async () => {
    setBusy("diarizing");
    setError(null);
    try {
      await diarizeMeeting(id, speakerCount());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      reload();
    }
  }, [id, reload, speakerCount]);

  const onRename = useCallback(
    async (raw: string, name: string) => {
      await renameSpeaker(id, raw, name || null);
      reload();
    },
    [id, reload],
  );

  const onConfirmPersona = useCallback(
    async (raw: string, personaId: number) => {
      await confirmSpeakerPersona(id, raw, personaId);
      reload();
      setPersonas(await listPersonas());
    },
    [id, reload],
  );

  const onUnlinkPersona = useCallback(
    async (raw: string) => {
      await unlinkSpeakerPersona(id, raw);
      reload();
    },
    [id, reload],
  );

  const onCreatePersona = useCallback(async (name: string): Promise<number> => {
    const pid = await createPersona(name);
    setPersonas(await listPersonas());
    return pid;
  }, []);

  const confirmDelete = useCallback(async () => {
    setDeleting(true);
    try {
      await deleteMeeting(id);
      onNavigate({ name: "home" });
    } catch (e) {
      setError(String(e));
    } finally {
      setDeleting(false);
      setConfirmDeleteOpen(false);
    }
  }, [id, onNavigate]);

  if (!meeting || meeting.id !== id) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        {error ?? ""}
      </div>
    );
  }

  const hasTranscript = meeting.segments.length > 0;
  const hasAudio = Boolean(meeting.micWav && meeting.systemWav);
  const needsDiarization =
    hasTranscript && hasAudio && meeting.segments.some((s) => s.source === "system" && !s.speaker);

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="space-y-2 border-b p-6 pt-4 pb-4">
        <div className="flex items-center justify-between">
          <button
            onClick={() => onNavigate({ name: "home" })}
            className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> Meetings
          </button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="size-7 text-muted-foreground"
                aria-label="More actions"
              >
                <MoreHorizontal className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem variant="destructive" onSelect={() => setConfirmDeleteOpen(true)}>
                <Trash2 /> Delete meeting
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        {titleDraft !== null ? (
          <input
            autoFocus
            value={titleDraft}
            onChange={(e) => setTitleDraft(e.target.value)}
            onBlur={commitTitle}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitTitle();
              if (e.key === "Escape") setTitleDraft(null);
            }}
            className="w-full rounded-md border bg-background px-2 py-1 text-lg font-semibold tracking-tight outline-none focus:border-ring"
          />
        ) : (
          <h1
            className="cursor-text text-lg font-semibold tracking-tight hover:opacity-80"
            title="Click to rename"
            onClick={() => setTitleDraft(meeting.title)}
          >
            {meeting.title}
          </h1>
        )}

        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <span>{fmtDate(meeting.startedAtMs)}</span>
          <span>{fmtDuration(meeting.startedAtMs, meeting.endedAtMs)}</span>
          {meeting.speakerCount > 0 && (
            <span className="inline-flex items-center gap-1">
              <Users className="size-3" />
              {meeting.speakerCount}
            </span>
          )}
          {!hasAudio && (
            <span className="rounded-full bg-secondary px-2 py-0.5">audio deleted</span>
          )}
        </div>

        <div className="flex items-center gap-2 pt-1 text-xs text-muted-foreground">
          <span>Customer</span>
          <div className="relative">
            <button
              disabled={assigning}
              onClick={() => setPickerOpen((o) => !o)}
              className="inline-flex h-7 max-w-56 items-center gap-1 rounded-md border bg-card px-1.5 text-xs outline-none hover:bg-accent focus:border-ring disabled:opacity-50"
            >
              <span className="truncate">
                {customers.find((c) => c.id === meeting.customerId)?.name ?? "Unassigned"}
              </span>
              <ChevronDown className="size-3 shrink-0 opacity-60" />
            </button>
            {pickerOpen && (
              <>
                {/* click-outside backdrop */}
                <button
                  aria-hidden
                  tabIndex={-1}
                  onClick={() => setPickerOpen(false)}
                  className="fixed inset-0 z-40 cursor-default"
                />
                <CustomerPicker
                  current={customers.find((c) => c.id === meeting.customerId) ?? null}
                  customers={customers}
                  onAssign={(cid) => assignCustomer(cid)}
                  onCreateCustomer={createCustomerInline}
                  onDismiss={() => setPickerOpen(false)}
                />
              </>
            )}
          </div>
          {assigning && <Loader2 className="size-3 animate-spin" />}
        </div>

        <div className="flex gap-2 pt-1">
          {!hasTranscript && hasAudio && (
            <Button size="sm" onClick={runTranscription} disabled={busy !== null}>
              {busy ? <Loader2 className="animate-spin" /> : <FileText />}
              {busy === "transcribing"
                ? "Transcribing…"
                : busy === "diarizing"
                  ? "Identifying speakers…"
                  : "Transcribe"}
            </Button>
          )}
          {hasTranscript && hasAudio && busy === null && (
            <div className="flex items-center gap-2">
              <Button size="sm" variant="outline" onClick={runDiarization}>
                <Users /> {needsDiarization ? "Identify speakers" : "Re-identify speakers"}
              </Button>
              <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
                Remote speakers:
                <select
                  value={numSpeakers}
                  onChange={(e) => setNumSpeakers(e.target.value)}
                  className="h-7 rounded-md border bg-card px-1.5 text-xs outline-none focus:border-ring"
                >
                  <option value="auto">Auto</option>
                  {[1, 2, 3, 4, 5, 6, 7, 8].map((n) => (
                    <option key={n} value={n}>
                      {n}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          )}
          {busy === "diarizing" && hasTranscript && (
            <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Identifying speakers…
            </span>
          )}
        </div>

        {error && <p className="text-sm text-destructive">{error}</p>}
      </div>

      {/* Tabs */}
      <div className="flex gap-1 border-b px-6 pt-2 pb-2">
        {(["review", "transcript"] as const).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={
              "rounded-md px-3 py-1 text-sm capitalize transition-colors " +
              (tab === t
                ? "bg-secondary font-medium text-secondary-foreground"
                : "text-muted-foreground hover:bg-accent/50 hover:text-foreground")
            }
          >
            {t}
          </button>
        ))}
      </div>

      {/* Body — both panes stay mounted so the notes editor keeps its buffer and
          the transcript keeps its scroll position when switching tabs. */}
      <div className="relative min-h-0 flex-1">
        {/* Review tab: Notes (left) + AI Summary (right) */}
        <div className={"absolute inset-0 flex " + (tab === "review" ? "" : "hidden")}>
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            <div className="flex items-center justify-between border-b px-4 h-11">
              <span className="text-xs font-medium text-muted-foreground">Notes</span>
              <span
                key={notesStatus}
                title={
                  notesStatus === "saved"
                    ? savedAtMs != null
                      ? `Saved at ${fmtSavedAt(savedAtMs)}`
                      : "Saved"
                    : undefined
                }
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
            </div>
            <div className="min-h-0 flex-1">
              <NotesEditor
                key={id}
                initialValue={meeting.notes ?? ""}
                onChange={onNotesMarkdown}
                onActivity={onNotesActivity}
              />
            </div>
          </div>
          <aside className="w-96 shrink-0 border-l bg-card/40">
            <SummaryPanel meetingId={id} hasTranscript={hasTranscript} />
          </aside>
        </div>

        {/* Transcript tab: audio player + full-width transcript */}
        <div className={"absolute inset-0 flex flex-col " + (tab === "transcript" ? "" : "hidden")}>
          {hasTranscript ? (
            <>
              {hasAudio && (
                <AudioPlayer
                  micWav={meeting.micWav!}
                  systemWav={meeting.systemWav!}
                  seek={seek}
                  onTimeUpdate={setCurrentMs}
                />
              )}
              <TranscriptPane
                segments={meeting.segments}
                renames={meeting.renames}
                onRenameSpeaker={onRename}
                speakerLinks={meeting.speakerLinks}
                personas={personas}
                onConfirmPersona={onConfirmPersona}
                onUnlinkPersona={onUnlinkPersona}
                onCreatePersona={onCreatePersona}
                onSeek={
                  hasAudio ? (ms) => setSeek((prev) => ({ ms, n: (prev?.n ?? 0) + 1 })) : undefined
                }
                currentMs={hasAudio ? (currentMs ?? undefined) : undefined}
                className="min-h-0 flex-1"
              />
            </>
          ) : (
            <div className="flex h-full items-center justify-center p-8 text-sm text-muted-foreground">
              {hasAudio
                ? "This meeting hasn't been transcribed yet."
                : "No transcript — the audio was deleted before transcription."}
            </div>
          )}
        </div>
      </div>

      <AlertDialog
        open={confirmDeleteOpen}
        onOpenChange={(open) => {
          if (!open && !deleting) setConfirmDeleteOpen(false);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete meeting?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently deletes{" "}
              <span className="font-medium text-foreground">{meeting.title}</span> and its audio.
              This cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={cn(buttonVariants({ variant: "destructive" }))}
              disabled={deleting}
              onClick={(e) => {
                e.preventDefault();
                confirmDelete();
              }}
            >
              {deleting ? <Loader2 className="animate-spin" /> : <Trash2 />}
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
