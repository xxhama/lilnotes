import { useCallback, useEffect, useState } from "react";
import { ArrowLeft, FileText, Loader2, Users } from "lucide-react";

import { Button } from "@/components/ui/button";
import SummaryPanel from "@/components/SummaryPanel";
import TranscriptPane from "@/components/TranscriptPane";
import {
  confirmSpeakerPersona,
  createPersona,
  diarizeMeeting,
  getMeeting,
  listPersonas,
  onSpeakersIdentified,
  onVoiceprintsEnrolled,
  renameSpeaker,
  transcribeMeeting,
  unlinkSpeakerPersona,
  updateMeetingTitle,
  type MeetingDetail as Meeting,
  type Persona,
} from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  meetingId: string;
  onNavigate: (route: Route) => void;
}

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

  const reload = useCallback(() => {
    getMeeting(id).then(setMeeting).catch((e) => setError(String(e)));
  }, [id]);

  useEffect(reload, [reload]);

  useEffect(() => {
    listPersonas().then(setPersonas).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onSpeakersIdentified(() => {
      if (!cancelled) reload();
    })
      .then((u) => {
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
      if (!cancelled) listPersonas().then(setPersonas).catch(() => {});
    })
      .then((u) => {
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

  if (!meeting) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        {error ?? ""}
      </div>
    );
  }

  const hasTranscript = meeting.segments.length > 0;
  const hasAudio = Boolean(meeting.micWav && meeting.systemWav);
  const needsDiarization =
    hasTranscript &&
    hasAudio &&
    meeting.segments.some((s) => s.source === "system" && !s.speaker);

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="space-y-2 border-b p-6 pt-12 pb-4">
        <button
          onClick={() => onNavigate({ name: "home" })}
          className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="size-3.5" /> Meetings
        </button>

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
          {!hasAudio && <span className="rounded-full bg-secondary px-2 py-0.5">audio deleted</span>}
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

      {/* Transcript + summary */}
      <div className="flex min-h-0 flex-1">
        <div className="min-w-0 flex-1">
          {hasTranscript ? (
            <TranscriptPane
              segments={meeting.segments}
              renames={meeting.renames}
              onRenameSpeaker={onRename}
              speakerLinks={meeting.speakerLinks}
              personas={personas}
              onConfirmPersona={onConfirmPersona}
              onUnlinkPersona={onUnlinkPersona}
              onCreatePersona={onCreatePersona}
              className="h-full"
            />
          ) : (
            <div className="flex h-full items-center justify-center p-8 text-sm text-muted-foreground">
              {hasAudio
                ? "This meeting hasn't been transcribed yet."
                : "No transcript — the audio was deleted before transcription."}
            </div>
          )}
        </div>
        <aside className="w-96 shrink-0 border-l bg-card/40">
          <SummaryPanel meetingId={id} hasTranscript={hasTranscript} />
        </aside>
      </div>
    </div>
  );
}
