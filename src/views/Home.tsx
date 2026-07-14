/**
 * Home / Meetings list view. Shows all persisted meetings with search
 * (matches titles and transcript text), delete, and navigation to the
 * meeting detail page. This is the default landing route.
 */
import { useCallback, useEffect, useState } from "react";
import { Loader2, Mic, Search, Trash2, Users } from "lucide-react";

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
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { deleteMeeting, listMeetings, type MeetingSummary } from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  onNavigate: (route: Route) => void;
}

function fmtDate(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function fmtDuration(ms: number | null): string {
  if (ms == null) return "";
  const min = Math.round(ms / 60000);
  if (min < 1) return "<1 min";
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)} h ${min % 60} min`;
}

/** Home: searchable meeting history. */
export default function HomeView({ onNavigate }: Props) {
  const [meetings, setMeetings] = useState<MeetingSummary[] | null>(null);
  const [search, setSearch] = useState("");
  /** Meeting pending deletion confirmation (null = dialog closed). */
  const [pendingDelete, setPendingDelete] = useState<MeetingSummary | null>(null);
  const [deleting, setDeleting] = useState(false);

  const refresh = useCallback((query: string) => {
    listMeetings(query || undefined)
      .then(setMeetings)
      .catch(() => setMeetings([]));
  }, []);

  useEffect(() => {
    const t = setTimeout(() => refresh(search), search ? 200 : 0);
    return () => clearTimeout(t);
  }, [search, refresh]);

  const confirmDelete = useCallback(async () => {
    if (!pendingDelete) return;
    setDeleting(true);
    try {
      await deleteMeeting(pendingDelete.id);
      setPendingDelete(null);
      refresh(search);
    } finally {
      setDeleting(false);
    }
  }, [pendingDelete, refresh, search]);

  if (meetings === null) {
    return <div className="p-8" />; // loading flash guard
  }

  if (meetings.length === 0 && !search) {
    return (
      <div className="mx-auto flex h-full max-w-2xl flex-col items-center justify-center gap-6 p-8 text-center">
        <div className="flex size-14 items-center justify-center rounded-2xl bg-secondary">
          <Mic className="size-6 text-muted-foreground" />
        </div>
        <div className="space-y-1.5">
          <h1 className="text-xl font-semibold tracking-tight">No meetings yet</h1>
          <p className="text-sm text-muted-foreground">
            Record your first meeting to see it here. Everything stays on this Mac.
          </p>
        </div>
        <Button onClick={() => onNavigate({ name: "recording" })}>
          <Mic /> Start recording
        </Button>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-3xl space-y-4 p-8 pt-4">
      <div className="flex items-center justify-between gap-4">
        <h1 className="text-lg font-semibold tracking-tight">Meetings</h1>
        <Button size="sm" onClick={() => onNavigate({ name: "recording" })}>
          <Mic /> Record
        </Button>
      </div>

      <div className="relative">
        <Search className="absolute top-2.5 left-3 size-4 text-muted-foreground" />
        <Input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search titles and transcripts…"
          className="h-9 w-full rounded-lg bg-card pr-3 pl-9 text-sm"
        />
      </div>

      {meetings.length === 0 ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
          No meetings match "{search}".
        </p>
      ) : (
        <div className="divide-y rounded-xl border bg-card">
          {meetings.map((m) => (
            <button
              key={m.id}
              onClick={() => onNavigate({ name: "meeting", meetingId: String(m.id) })}
              className="group flex w-full items-center gap-4 p-4 text-left transition-colors hover:bg-accent/50"
            >
              <div className="min-w-0 flex-1 space-y-0.5">
                <div className="truncate text-sm font-medium">{m.title}</div>
                <div className="flex items-center gap-3 text-xs text-muted-foreground">
                  <span>{fmtDate(m.startedAtMs)}</span>
                  {m.durationMs != null && <span>{fmtDuration(m.durationMs)}</span>}
                  {m.speakerCount > 0 && (
                    <span className="inline-flex items-center gap-1">
                      <Users className="size-3" />
                      {m.speakerCount}
                    </span>
                  )}
                  {m.segmentCount === 0 && <span className="text-amber-600">not transcribed</span>}
                </div>
                {m.preview && (
                  <p className="truncate text-xs text-muted-foreground/70">{m.preview}</p>
                )}
              </div>
              <Button
                size="icon"
                variant="ghost"
                className="size-8 opacity-0 transition-opacity group-hover:opacity-100"
                onClick={(e) => {
                  e.stopPropagation();
                  setPendingDelete(m);
                }}
                aria-label="Delete meeting"
                asChild
              >
                <span>
                  <Trash2 className="size-4 text-muted-foreground" />
                </span>
              </Button>
            </button>
          ))}
        </div>
      )}

      <AlertDialog
        open={pendingDelete != null}
        onOpenChange={(open) => {
          if (!open && !deleting) setPendingDelete(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete meeting?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently deletes{" "}
              <span className="font-medium text-foreground">{pendingDelete?.title}</span> and its
              audio. This cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={cn(buttonVariants({ variant: "destructive" }))}
              disabled={deleting}
              // Prevent radix's auto-close so the dialog stays open while the
              // delete runs; we close it ourselves on success.
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
