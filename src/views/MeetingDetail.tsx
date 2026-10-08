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
import { Input } from "@/components/ui/input";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import AudioPlayer from "@/components/AudioPlayer";
import NotesEditor from "@/components/NotesEditor";
import TranscriptToolbar from "@/components/TranscriptToolbar";
import PickerCombobox from "@/components/PickerCombobox";
import SummaryPanel from "@/components/SummaryPanel";
import TranscriptPane from "@/components/TranscriptPane";
import { cn } from "@/lib/utils";
import {
  confirmSpeakerPersona,
  createCustomer,
  createPersona,
  deleteMeeting,
  deleteSegment,
  diarizeMeeting,
  getMeeting,
  listCustomers,
  listHiddenSegments,
  listPersonas,
  markSegmentEcho,
  cleanEcho,
  cleanEchoSegment,
  onOfflineAecProgress,
  onSpeakersIdentified,
  onVoiceprintsEnrolled,
  listAsrModels,
  preparePlaybackAudio,
  renameSpeaker,
  retranscribeMeeting,
  restoreSegment,
  revertEchoClean,
  setMeetingCustomer,
  speakerPersonaCandidates,
  transcribeMeeting,
  unmarkSegmentEcho,
  unlinkSpeakerPersona,
  updateMeetingNotes,
  updateMeetingTitle,
  type AsrModelInfo,
  type CustomerSummary,
  type MeetingDetail as Meeting,
  type Persona,
  type PlaybackAudio,
  type SpeakerCandidates,
  type TranscriptSegment,
} from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  meetingId: string;
  /** When true (set by the Record-page stop flow), auto-start speaker
   * identification on load if the meeting still needs it. Falsy when opened
   * from Home/CustomerDetail, so older meetings keep the manual button. */
  autoDiarize?: boolean;
  /** Customer id this meeting was opened from (set by CustomerDetail). When
   * present, the header back button returns to that customer (scrolled to its
   * Meetings section) instead of the Meetings list. */
  fromCustomerId?: string;
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
export default function MeetingDetailView({
  meetingId,
  autoDiarize,
  fromCustomerId,
  onNavigate,
}: Props) {
  const id = Number(meetingId);
  const [meeting, setMeeting] = useState<Meeting | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"transcribing" | "diarizing" | "cleaning" | null>(null);
  /** Offline AEC progress fraction (0..1) while `busy === "cleaning"`. */
  const [aecPct, setAecPct] = useState<number | null>(null);
  const [titleDraft, setTitleDraft] = useState<string | null>(null);
  /** "auto" or a declared remote-speaker count for re-identification. */
  const [numSpeakers, setNumSpeakers] = useState<string>("auto");
  const [personas, setPersonas] = useState<Persona[]>([]);
  /** Voice matches + customer roster that rank the speaker picker. */
  const [candidates, setCandidates] = useState<SpeakerCandidates | null>(null);
  /** Bumped on speakers:identified (fresh embeddings) to refetch candidates. */
  const [identifiedTick, setIdentifiedTick] = useState(0);
  const [customers, setCustomers] = useState<CustomerSummary[]>([]);
  const [assigning, setAssigning] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [tab, setTab] = useState<"review" | "transcript">("review");
  /** When true, the transcript tab shows echo-marked + soft-deleted segments
   * (fetched via `listHiddenSegments`) with Unmark/Restore actions. */
  const [showHidden, setShowHidden] = useState(false);
  const [hiddenSegments, setHiddenSegments] = useState<TranscriptSegment[] | null>(null);
  /** Seek request (ms from recording start) sent to the audio player when the
   * user clicks a transcript timestamp. A new value (even equal to the prior
   * one) re-triggers the seek; use a counter+ms pair so repeated clicks on the
   * same segment still replay. */
  const [seek, setSeek] = useState<{ ms: number; n: number } | null>(null);
  /** Player sources from `preparePlaybackAudio` (constant-bitrate WAVs the
   * webview can seek exactly). Null until prepared for the current meeting;
   * the player renders in its loading state meanwhile. */
  const [playback, setPlayback] = useState<PlaybackAudio | null>(null);
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
  /** Available whisper models (drives the re-transcribe dropdown). Downloaded
   * models only are listed; the active flag marks the global live model. */
  const [asrModels, setAsrModels] = useState<AsrModelInfo[]>([]);
  /** Selected model id for the re-transcribe dropdown. Defaults to the meeting's
   * recorded model, else the global active model, else the first downloaded. */
  const [retranscribeModel, setRetranscribeModel] = useState<string>("");
  /** Re-transcribe confirmation dialog. */
  const [confirmRetranscribeOpen, setConfirmRetranscribeOpen] = useState(false);
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

  // Prepare the player's sources whenever the meeting (re)loads. Keyed on the
  // meeting object so every reload after a clean-echo / revert / diarize
  // re-checks the files; the backend keys cache names on the source files'
  // mtime, so unchanged sources yield the same paths and `prev` is kept (no
  // <audio> reload on a rename or notes save). Skipped while busy so the
  // cleaned mic is never decoded mid-write.
  useEffect(() => {
    if (!meeting || meeting.id !== id || !meeting.micWav || !meeting.systemWav) {
      setPlayback(null);
      return;
    }
    if (busy !== null) return;
    let cancelled = false;
    preparePlaybackAudio(id)
      .then((p) => {
        if (cancelled) return;
        setPlayback((prev) =>
          prev && prev.micWav === p.micWav && prev.systemWav === p.systemWav ? prev : p,
        );
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [meeting, busy, id]);

  // Load the whisper model registry once (drives the re-transcribe dropdown).
  useEffect(() => {
    listAsrModels()
      .then(setAsrModels)
      .catch((e) => setError(String(e)));
  }, []);

  // Default the re-transcribe dropdown to the meeting's recorded model, else
  // the global active model, else the first downloaded model. Re-derives when
  // the meeting changes or the model list loads so switching meetings resets it.
  useEffect(() => {
    if (asrModels.length === 0) return;
    const defaultId =
      meeting?.asrModel ??
      asrModels.find((m) => m.active)?.id ??
      asrModels.find((m) => m.downloaded)?.id ??
      "";
    setRetranscribeModel(defaultId);
  }, [meeting?.asrModel, asrModels]);

  // Eagerly fetch the full segment set (incl. echo / deleted) and keep it
  // cached so toggling "show hidden" is instant — no empty flash while the
  // fetch is in flight. Keyed on the meeting object so every reload (after a
  // mutation, retranscribe, diarize, clean, rename, …) re-fetches the cache;
  // when there are no hidden segments, drop it.
  const refreshHidden = useCallback(() => {
    listHiddenSegments(id)
      .then(setHiddenSegments)
      .catch((e) => setError(String(e)));
  }, [id]);
  useEffect(() => {
    if (!meeting) return;
    if (meeting.hiddenSegmentCount > 0) refreshHidden();
    else setHiddenSegments(null);
  }, [meeting, refreshHidden]);

  /** Apply a per-segment mutation to every id in a group, then reload so the
   *  transcript + hidden count reflect the new state. Mutations are sequential
   *  (small N — a grouped run); reload fires after they all commit. The
   *  meeting-keyed effect above re-fetches the hidden cache once reload lands. */
  const mutateSegments = useCallback(
    (ids: number[], fn: (segmentId: number) => Promise<void>) => {
      (async () => {
        for (const sid of ids) {
          try {
            await fn(sid);
          } catch (e) {
            setError(String(e));
          }
        }
        reload();
      })();
    },
    [reload],
  );

  const onMarkEcho = useCallback(
    (ids: number[]) => mutateSegments(ids, markSegmentEcho),
    [mutateSegments],
  );
  const onDeleteSegment = useCallback(
    (ids: number[]) => mutateSegments(ids, deleteSegment),
    [mutateSegments],
  );
  const onUnmarkEcho = useCallback(
    (ids: number[]) => mutateSegments(ids, unmarkSegmentEcho),
    [mutateSegments],
  );
  const onRestoreSegment = useCallback(
    (ids: number[]) => mutateSegments(ids, restoreSegment),
    [mutateSegments],
  );

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
    setPlayback(null);
    setShowHidden(false);
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

  // Re-rank the speaker picker whenever its inputs change: the meeting's
  // customer (roster), persona galleries (`personas` is refetched after every
  // confirm / unlink / create / enrollment), or fresh speaker embeddings.
  // Best-effort: on failure the picker falls back to the alphabetical list.
  const customerId = meeting?.customerId;
  useEffect(() => {
    let cancelled = false;
    speakerPersonaCandidates(id)
      .then((c) => {
        if (!cancelled) setCandidates(c);
      })
      .catch(() => {
        if (!cancelled) setCandidates(null);
      });
    return () => {
      cancelled = true;
    };
  }, [id, customerId, personas, identifiedTick]);

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
      if (cancelled) return;
      reload();
      setIdentifiedTick((n) => n + 1);
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

  // Re-transcribe with the per-meeting selected model (does transcribe +
  // diarize server-side in one call). Does not change the global/live model.
  const runRetranscribe = useCallback(async () => {
    if (!retranscribeModel) return;
    setBusy("transcribing");
    setError(null);
    try {
      await retranscribeMeeting(id, retranscribeModel);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      reload();
    }
  }, [id, retranscribeModel, reload]);

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

  /** Run offline AEC on the mic using the system recording as the reference (seeded by
   *  echo-marked mic segments), then re-transcribe + re-diarize. The original
   *  mic file is preserved; `meeting.micCleanedWav` points at the cleaned copy. */
  const runCleanEcho = useCallback(async () => {
    setBusy("cleaning");
    setAecPct(0);
    setError(null);
    try {
      await cleanEcho(id);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      setAecPct(null);
      reload();
    }
  }, [id, reload]);

  /** Drop the cleaned mic, re-transcribe from the original mic recording. */
  const runRevertEchoClean = useCallback(async () => {
    setBusy("cleaning");
    setAecPct(null);
    setError(null);
    try {
      await revertEchoClean(id);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      setAecPct(null);
      reload();
    }
  }, [id, reload]);

  /** Per-segment offline echo clean: cancel echo in one mic group's
   *  `[startMs, endMs]` only (learns from marked echo regions; leaves the rest
   *  of the mic bit-identical), then re-transcribe + re-diarize. Reuses the
   *  same `busy:"cleaning"` + `aecPct` + progress subscription as the
   *  whole-meeting clean. */
  const onCleanEchoRange = useCallback(
    async (startMs: number, endMs: number) => {
      if (busy) return;
      setBusy("cleaning");
      setAecPct(0);
      setError(null);
      try {
        await cleanEchoSegment(id, startMs, endMs);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(null);
        setAecPct(null);
        reload();
      }
    },
    [busy, id, reload],
  );

  // Subscribe to offline AEC progress events for this meeting while a clean is
  // running. The Rust side emits 0..=1 fractions under spawn_blocking.
  useEffect(() => {
    if (busy !== "cleaning") return;
    let active = true;
    const unlisten = onOfflineAecProgress((e) => {
      if (active && e.meetingId === id) setAecPct(e.pct);
    });
    return () => {
      active = false;
      void unlisten.then((fn) => fn());
    };
  }, [busy, id]);

  // Auto-start speaker identification on load when arriving from a just-ended
  // recording (autoDiarize). Gated on needsDiarization so it only fires when
  // there's an unlabeled system segment, and ref-guarded per meeting id so it
  // runs at most once (survives StrictMode double-invoke + the async busy flip).
  // When autoDiarize is falsy (opened from Home/CustomerDetail), older meetings
  // keep the manual "Identify speakers" button + Remote-speakers count picker.
  const autoDiarizedFor = useRef<number | null>(null);
  useEffect(() => {
    if (!autoDiarize || !meeting) return;
    if (autoDiarizedFor.current === meeting.id) return;
    if (busy !== null) return;
    const hasAudio = Boolean(meeting.micWav && meeting.systemWav);
    const needsDiarization =
      meeting.segments.length > 0 &&
      hasAudio &&
      meeting.segments.some((s) => s.source === "system" && !s.speaker);
    if (!needsDiarization) return;
    autoDiarizedFor.current = meeting.id;
    runDiarization();
  }, [autoDiarize, meeting, busy, runDiarization]);

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
      // Unlinking also drops the voiceprint enrolled from this speaker, so
      // persona print counts change.
      setPersonas(await listPersonas());
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
  // Timestamp clicks are inert (not swallowed) until the sources are prepared.
  const playerReady = hasAudio && playback !== null;
  const needsDiarization =
    hasTranscript && hasAudio && meeting.segments.some((s) => s.source === "system" && !s.speaker);
  const currentCustomer = customers.find((c) => c.id === meeting.customerId) ?? null;
  // Downloaded models populate the re-transcribe dropdown (active = global live).
  const downloadedModels = asrModels.filter((m) => m.downloaded);
  // The reset value for the toolbar's Model select — the meeting's recorded
  // model, else the global live model, else the first downloaded. Drives the
  // Settings popover's dirty dot and matches the retranscribeModel default.
  const defaultModelId =
    meeting.asrModel ??
    asrModels.find((m) => m.active)?.id ??
    asrModels.find((m) => m.downloaded)?.id ??
    "";
  // Human-readable label for the model that produced this transcript, if known.
  const transcribedWithLabel =
    meeting.asrModel != null
      ? (asrModels.find((m) => m.id === meeting.asrModel)?.label ?? meeting.asrModel)
      : null;

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="space-y-2 border-b p-6 pt-4 pb-4">
        <div className="flex items-center justify-between">
          {fromCustomerId ? (
            <button
              onClick={() =>
                onNavigate({
                  name: "customer",
                  customerId: fromCustomerId,
                  focusMeetings: true,
                })
              }
              className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="size-3.5" /> {currentCustomer?.name ?? "Customer"}
            </button>
          ) : (
            <button
              onClick={() => onNavigate({ name: "home" })}
              className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="size-3.5" /> Meetings
            </button>
          )}
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
          <Input
            autoFocus
            value={titleDraft}
            onChange={(e) => setTitleDraft(e.target.value)}
            onBlur={commitTitle}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitTitle();
              if (e.key === "Escape") setTitleDraft(null);
            }}
            className="w-full rounded-md bg-background px-2 py-1 text-lg font-semibold tracking-tight"
          />
        ) : (
          <Tooltip>
            <TooltipTrigger asChild>
              <h1
                tabIndex={0}
                className="cursor-text text-lg font-semibold tracking-tight hover:opacity-80"
                onClick={() => setTitleDraft(meeting.title)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    setTitleDraft(meeting.title);
                  }
                }}
              >
                {meeting.title}
              </h1>
            </TooltipTrigger>
            <TooltipContent>Click to rename</TooltipContent>
          </Tooltip>
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
          {transcribedWithLabel && (
            <span className="inline-flex items-center gap-1">
              <FileText className="size-3" />
              Transcribed with: {transcribedWithLabel}
            </span>
          )}
          {!hasAudio && (
            <span className="rounded-full bg-secondary px-2 py-0.5">audio deleted</span>
          )}
        </div>

        <div className="flex items-center gap-2 pt-1 text-xs text-muted-foreground">
          <span>Customer</span>
          <PickerCombobox
            open={pickerOpen}
            onOpenChange={setPickerOpen}
            trigger={
              <button
                disabled={assigning}
                className="flex w-fit max-w-56 items-center justify-between gap-2 rounded-md border border-input bg-card px-3 py-2 text-xs whitespace-nowrap shadow-xs transition-[color,box-shadow] outline-none dark:bg-input/30 dark:hover:bg-input/50 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 data-[placeholder]:text-muted-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 [&_svg:not([class*='text-'])]:text-muted-foreground"
              >
                <span
                  className="truncate data-[placeholder]:text-muted-foreground"
                  data-placeholder={currentCustomer ? undefined : ""}
                >
                  {currentCustomer?.name ?? "Unassigned"}
                </span>
                <ChevronDown className="size-4 opacity-50" />
              </button>
            }
            items={customers
              .filter((c) => c.id !== meeting.customerId)
              .map((c) => ({ id: c.id, label: c.name, sublabel: `${c.meetingCount} mtgs` }))}
            currentId={meeting.customerId ?? null}
            currentLabel={currentCustomer?.name}
            currentSublabel={currentCustomer ? `${currentCustomer.meetingCount} mtgs` : undefined}
            onPick={assignCustomer}
            onCreate={createCustomerInline}
            onUnassign={meeting.customerId != null ? () => assignCustomer(null) : undefined}
            unassignLabel="Unassign customer"
            createNoun="customer"
            placeholder="Search customers…"
          />
          {assigning && <Loader2 className="size-3 animate-spin" />}
        </div>

        <div className="flex items-center gap-2 pt-1">
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
          {/* Cross-tab awareness: a transcript action running while the user is
           * on the Review tab. The Transcript tab shows rich progress in its own
           * toolbar; this compact pill just says "something is running". */}
          {hasTranscript && busy !== null && tab !== "transcript" && (
            <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" />
              {busy === "diarizing"
                ? "Identifying speakers…"
                : busy === "transcribing"
                  ? "Re-transcribing…"
                  : "Cleaning echo…"}
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
              <Tooltip>
                <TooltipTrigger asChild>
                  <span
                    key={notesStatus}
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
                </TooltipTrigger>
                {notesStatus === "saved" && (
                  <TooltipContent>
                    {savedAtMs != null ? `Saved at ${fmtSavedAt(savedAtMs)}` : "Saved"}
                  </TooltipContent>
                )}
              </Tooltip>
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
            <SummaryPanel
              meetingId={id}
              hasTranscript={hasTranscript}
              onTitleGenerated={(title) => {
                // The backend already persisted the AI title; just reflect it in
                // local state so the <h1> updates without a full reload. Skip if
                // the user is mid-rename so their draft isn't clobbered.
                if (titleDraft === null) setMeeting((m) => (m ? { ...m, title } : m));
              }}
            />
          </aside>
        </div>

        {/* Transcript tab: toolbar + full-width transcript (audio player is
            page-level, above the tabs) */}
        <div className={"absolute inset-0 flex flex-col " + (tab === "transcript" ? "" : "hidden")}>
          {hasTranscript ? (
            <>
              {/* Transcript toolbar — operations | Settings popover | view
               * toggle. Presentational; all state/handlers live in this view. */}
              {hasTranscript && (hasAudio || meeting.hiddenSegmentCount > 0) && (
                <TranscriptToolbar
                  hasAudio={hasAudio}
                  busy={busy}
                  aecPct={aecPct}
                  needsDiarization={needsDiarization}
                  hiddenSegmentCount={meeting.hiddenSegmentCount}
                  hasCleanedMic={Boolean(meeting.micCleanedWav)}
                  downloadedModels={downloadedModels}
                  retranscribeModel={retranscribeModel}
                  onRetranscribeModelChange={setRetranscribeModel}
                  defaultModelId={defaultModelId}
                  numSpeakers={numSpeakers}
                  onNumSpeakersChange={setNumSpeakers}
                  showHidden={showHidden}
                  onToggleHidden={() => setShowHidden((v) => !v)}
                  onRetranscribe={() => setConfirmRetranscribeOpen(true)}
                  onReidentify={runDiarization}
                  onCleanEcho={runCleanEcho}
                  onRevertEchoClean={runRevertEchoClean}
                />
              )}
              <TranscriptPane
                segments={showHidden ? (hiddenSegments ?? []) : meeting.segments}
                renames={meeting.renames}
                onRenameSpeaker={onRename}
                speakerLinks={meeting.speakerLinks}
                personas={personas}
                candidates={candidates ?? undefined}
                onConfirmPersona={onConfirmPersona}
                onUnlinkPersona={onUnlinkPersona}
                onCreatePersona={onCreatePersona}
                onSeek={
                  playerReady
                    ? (ms) => setSeek((prev) => ({ ms, n: (prev?.n ?? 0) + 1 }))
                    : undefined
                }
                currentMs={playerReady ? (currentMs ?? undefined) : undefined}
                onMarkEcho={onMarkEcho}
                onCleanEchoRange={onCleanEchoRange}
                onDeleteSegment={onDeleteSegment}
                onUnmarkEcho={onUnmarkEcho}
                onRestoreSegment={onRestoreSegment}
                showHidden={showHidden}
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

      {/* Bottom-docked audio player — last row of the detail-pane flex column,
          so the Body (flex-1) above it shrinks to fit and nothing scrolls
          behind it. Visible on both Review and Transcript at every scroll
          position. Single instance; lifted playback state (seek/currentMs)
          and tab-switch survival are unchanged. Gated on hasAudio only; the
          sources arrive from `preparePlaybackAudio` (null = still decoding). */}
      {hasAudio && (
        <AudioPlayer
          micWav={playback?.micWav ?? null}
          systemWav={playback?.systemWav ?? null}
          seek={seek}
          onTimeUpdate={setCurrentMs}
        />
      )}

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

      <AlertDialog
        open={confirmRetranscribeOpen}
        onOpenChange={(open) => {
          if (!open && busy !== "transcribing") setConfirmRetranscribeOpen(false);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Re-transcribe with{" "}
              {asrModels.find((m) => m.id === retranscribeModel)?.label ?? retranscribeModel}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This replaces the current transcript and speaker labels using the selected model. Echo
              and delete marks are preserved. This does not change the model used for future
              recordings.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={busy === "transcribing"}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              disabled={busy === "transcribing"}
              onClick={(e) => {
                e.preventDefault();
                setConfirmRetranscribeOpen(false);
                runRetranscribe();
              }}
            >
              {busy === "transcribing" ? <Loader2 className="animate-spin" /> : <FileText />}
              Re-transcribe
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
