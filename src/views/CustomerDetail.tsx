/**
 * Customer detail view. Shows a customer's meetings (with search), AI
 * summaries, notes, and supports merge with another customer. Reached
 * from the Customers list.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  Building2,
  Calendar,
  Clock,
  Loader2,
  ListTodo,
  Merge,
  Search,
  Trash2,
  Users,
} from "lucide-react";

import CustomerAvatar from "@/components/CustomerAvatar";
import CustomerSummaryPanel from "@/components/CustomerSummaryPanel";
import CustomerTasksSection from "@/components/CustomerTasksSection";
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
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import {
  deleteCustomer,
  getCustomer,
  listCustomers,
  mergeCustomers,
  renameCustomer,
  searchCustomerMeetings,
  updateCustomerNotes,
  type CustomerDetail as Customer,
  type CustomerSearchResult,
  type CustomerSummary,
} from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  customerId: string;
  /** Scroll to the Meetings section on mount (set by MeetingDetail's back
   * button when returning from a meeting opened from this customer). */
  focusMeetings?: boolean;
  onNavigate: (route: Route) => void;
}

function fmtDate(ms: number | null): string {
  if (ms == null) return "—";
  return new Date(ms).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

function fmtDateTime(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function fmtDuration(ms: number | null): string {
  if (ms == null) return "—";
  const min = Math.round(ms / 60000);
  if (min < 1) return "<1 min";
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)} h ${min % 60} min`;
}

const FIELD_LABELS: Record<string, string> = {
  title: "Title",
  transcript: "Transcript",
  summary: "Summary",
  notes: "Notes",
  persona: "Persona",
};

/** Customer detail: birds-eye view of one account. */
export default function CustomerDetailView({ customerId, focusMeetings, onNavigate }: Props) {
  const id = Number(customerId);
  const [customer, setCustomer] = useState<Customer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [titleDraft, setTitleDraft] = useState<string | null>(null);

  // Notes (plain textarea, debounced autosave).
  const [notesDraft, setNotesDraft] = useState("");
  const [notesStatus, setNotesStatus] = useState<"saved" | "modified" | "saving">("saved");
  const notesTimer = useRef<number | null>(null);
  const notesGen = useRef(0);

  // Scoped search.
  const [search, setSearch] = useState("");
  const [results, setResults] = useState<CustomerSearchResult[] | null>(null);
  const [searching, setSearching] = useState(false);

  // Merge + delete dialogs.
  const [allCustomers, setAllCustomers] = useState<CustomerSummary[]>([]);
  const [mergeOpen, setMergeOpen] = useState(false);
  const [mergePick, setMergePick] = useState<number | null>(null);
  const [merging, setMerging] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  // Auto-scroll target when returning from a meeting opened from this customer
  // (focusMeetings set by MeetingDetail's back button).
  const meetingsRef = useRef<HTMLDivElement>(null);
  const didFocus = useRef(false);

  const reload = useCallback(() => {
    getCustomer(id)
      .then((c) => {
        setCustomer(c);
        setNotesDraft(c.notes ?? "");
        setNotesStatus("saved");
      })
      .catch((e) => setError(String(e)));
  }, [id]);

  useEffect(() => {
    reload();
  }, [reload]);

  // One-shot: when arriving with focusMeetings (returning from a meeting
  // opened from this customer), scroll the Meetings section into view once
  // the customer data has loaded. The didFocus guard prevents later reloads
  // (rename/notes save) from re-jumping the scroll position.
  useEffect(() => {
    if (!focusMeetings || didFocus.current || !customer) return;
    didFocus.current = true;
    // Defer one frame so the Radix ScrollArea viewport has laid out.
    requestAnimationFrame(() => meetingsRef.current?.scrollIntoView({ block: "start" }));
  }, [focusMeetings, customer]);

  // Reset notes buffer on customer change (navigating between customers).
  useEffect(() => {
    notesGen.current = 0;
    if (notesTimer.current) {
      clearTimeout(notesTimer.current);
      notesTimer.current = null;
    }
  }, [id]);

  const commitTitle = useCallback(async () => {
    if (titleDraft == null) return;
    const name = titleDraft.trim();
    if (name && customer && name !== customer.name) {
      try {
        await renameCustomer(id, name);
        reload();
      } catch (e) {
        setError(String(e));
      }
    }
    setTitleDraft(null);
  }, [titleDraft, customer, id, reload]);

  const scheduleNotesSave = useCallback(
    (value: string) => {
      setNotesStatus("modified");
      const gen = ++notesGen.current;
      if (notesTimer.current) clearTimeout(notesTimer.current);
      notesTimer.current = window.setTimeout(async () => {
        setNotesStatus("saving");
        try {
          await updateCustomerNotes(id, value.trim() ? value : null);
          if (gen === notesGen.current) setNotesStatus("saved");
        } catch (e) {
          setError(String(e));
          if (gen === notesGen.current) setNotesStatus("modified");
        }
      }, 600);
    },
    [id],
  );

  // Debounced scoped search (matches Home.tsx cadence).
  useEffect(() => {
    if (!search.trim()) {
      setResults(null);
      setSearching(false);
      return;
    }
    setSearching(true);
    const t = setTimeout(() => {
      searchCustomerMeetings(id, search)
        .then((r) => setResults(r))
        .catch((e) => setError(String(e)))
        .finally(() => setSearching(false));
    }, 200);
    return () => clearTimeout(t);
  }, [search, id]);

  const openMerge = useCallback(async () => {
    try {
      const list = await listCustomers();
      setAllCustomers(list.filter((c) => c.id !== id));
      setMergePick(null);
      setMergeOpen(true);
    } catch (e) {
      setError(String(e));
    }
  }, [id]);

  const confirmMerge = useCallback(async () => {
    if (mergePick == null) return;
    setMerging(true);
    try {
      await mergeCustomers(mergePick, id); // merge picked (source) into current (target)
      setMergeOpen(false);
      reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setMerging(false);
    }
  }, [mergePick, id, reload]);

  const confirmDelete = useCallback(async () => {
    setDeleting(true);
    try {
      await deleteCustomer(id);
      onNavigate({ name: "customers" });
    } catch (e) {
      setError(String(e));
    } finally {
      setDeleting(false);
    }
  }, [id, onNavigate]);

  if (!customer || customer.id !== id) {
    return <div className="p-8 text-sm text-muted-foreground">{error ?? "Loading…"}</div>;
  }

  const stats = [
    { icon: Calendar, label: "Meetings", value: String(customer.meetingCount) },
    { icon: Clock, label: "Last contact", value: fmtDate(customer.lastMeetingAtMs) },
    { icon: Calendar, label: "First contact", value: fmtDate(customer.firstMeetingAtMs) },
    { icon: Users, label: "Roster", value: String(customer.personaRoster.length) },
    { icon: Clock, label: "Total time", value: fmtDuration(customer.totalDurationMs) },
    { icon: ListTodo, label: "Open tasks", value: String(customer.openTaskCount) },
  ];

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="space-y-2 border-b p-6 pt-4 pb-4">
        <div className="flex items-center justify-between">
          <button
            onClick={() => onNavigate({ name: "customers" })}
            className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> Customers
          </button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="size-7 text-muted-foreground">
                <Merge className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={openMerge}>
                <Merge /> Merge with…
              </DropdownMenuItem>
              <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                <Trash2 /> Delete customer
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        <div className="flex items-center gap-3">
          <CustomerAvatar name={customer.name} className="size-12 text-lg" />
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
              className="flex-1 rounded-md bg-background px-2 py-1 text-lg font-semibold tracking-tight"
            />
          ) : (
            <Tooltip>
              <TooltipTrigger asChild>
                <h1
                  tabIndex={0}
                  className="flex-1 cursor-text text-lg font-semibold tracking-tight hover:opacity-80"
                  onClick={() => setTitleDraft(customer.name)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setTitleDraft(customer.name);
                    }
                  }}
                >
                  {customer.name}
                </h1>
              </TooltipTrigger>
              <TooltipContent>Click to rename</TooltipContent>
            </Tooltip>
          )}
        </div>
        <p className="text-xs text-muted-foreground">
          Last met {fmtDate(customer.lastMeetingAtMs)}
        </p>

        {/* Quick stats strip */}
        <div className="grid grid-cols-2 gap-2 pt-2 sm:grid-cols-3 lg:grid-cols-6">
          {stats.map(({ icon: Icon, label, value }) => (
            <div key={label} className="rounded-lg border bg-card p-2.5">
              <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
                <Icon className="size-3" />
                {label}
              </div>
              <div className="mt-0.5 truncate text-sm font-medium">{value}</div>
            </div>
          ))}
        </div>
      </div>

      {/* Body: left = roster + meetings + search; right = rollup panel */}
      <div className="flex min-h-0 flex-1">
        <ScrollArea className="min-w-0 flex-1">
          <div className="p-6 space-y-6">
            {error && <p className="text-sm text-destructive">{error}</p>}

            {/* Scoped search */}
            <section className="space-y-1.5">
              <h2 className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                <Search className="size-3.5" /> Search within {customer.name}
              </h2>
              <div className="relative">
                <Search className="absolute top-2.5 left-3 size-4 text-muted-foreground" />
                <Input
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                  placeholder="Search transcripts, titles, summaries, notes, personas…"
                  className="h-9 w-full rounded-lg bg-card pr-3 pl-9 text-sm"
                />
                {searching && (
                  <Loader2 className="absolute top-2.5 right-3 size-4 animate-spin text-muted-foreground" />
                )}
              </div>
              {results !== null &&
                (results.length === 0 ? (
                  <p className="py-4 text-center text-xs text-muted-foreground">
                    No matches across {customer.name}'s meetings for "{search}".
                  </p>
                ) : (
                  <div className="divide-y rounded-lg border bg-card">
                    {results.map((r) => (
                      <button
                        key={r.meetingId}
                        onClick={() =>
                          onNavigate({
                            name: "meeting",
                            meetingId: String(r.meetingId),
                            fromCustomerId: String(id),
                          })
                        }
                        className="block w-full p-3 text-left transition-colors hover:bg-accent/50"
                      >
                        <div className="truncate text-sm font-medium">{r.title}</div>
                        <div className="text-[11px] text-muted-foreground">
                          {fmtDateTime(r.startedAtMs)}
                        </div>
                        <div className="mt-1 flex flex-wrap gap-1.5">
                          {r.hits.map((h, i) => (
                            <Tooltip key={i}>
                              <TooltipTrigger asChild>
                                <span
                                  tabIndex={0}
                                  className="inline-flex items-center gap-1 rounded-md bg-secondary px-1.5 py-0.5 text-[11px] text-muted-foreground"
                                >
                                  <span className="font-medium text-foreground/70">
                                    {FIELD_LABELS[h.field] ?? h.field}
                                  </span>
                                  <span className="max-w-72 truncate">{h.snippet}</span>
                                </span>
                              </TooltipTrigger>
                              <TooltipContent>{h.snippet}</TooltipContent>
                            </Tooltip>
                          ))}
                        </div>
                      </button>
                    ))}
                  </div>
                ))}
            </section>

            {/* Meeting list */}
            <section ref={meetingsRef} className="space-y-1.5 scroll-mt-4">
              <h2 className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                <Building2 className="size-3.5" /> Meetings ({customer.meetings.length})
              </h2>
              {customer.meetings.length === 0 ? (
                <p className="rounded-lg border bg-card p-3 text-xs text-muted-foreground">
                  No meetings assigned to this customer yet. Assign one from a meeting's "Customer"
                  selector.
                </p>
              ) : (
                <div className="divide-y rounded-lg border bg-card">
                  {customer.meetings.map((m) => (
                    <button
                      key={m.id}
                      onClick={() =>
                        onNavigate({
                          name: "meeting",
                          meetingId: String(m.id),
                          fromCustomerId: String(id),
                        })
                      }
                      className="group flex w-full items-center gap-3 p-3 text-left transition-colors hover:bg-accent/50"
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
                        </div>
                        {m.preview && (
                          <p className="truncate text-xs text-muted-foreground/70">{m.preview}</p>
                        )}
                      </div>
                    </button>
                  ))}
                </div>
              )}
            </section>

            {/* Tasks (scoped to this customer) */}
            <CustomerTasksSection customerId={id} onNavigate={onNavigate} />

            {/* Persona roster (derived) */}
            <section className="space-y-1.5">
              <h2 className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                <Users className="size-3.5" /> Persona roster
              </h2>
              {customer.personaRoster.length === 0 ? (
                <p className="rounded-lg border bg-card p-3 text-xs text-muted-foreground">
                  No confirmed personas in this customer's meetings yet.
                </p>
              ) : (
                <div className="divide-y rounded-lg border bg-card">
                  {customer.personaRoster.map((p) => (
                    <div
                      key={p.personaId}
                      className="flex items-center justify-between gap-3 p-2.5"
                    >
                      <span className="truncate text-sm font-medium">{p.displayName}</span>
                      <span className="shrink-0 text-xs text-muted-foreground">
                        {p.meetingCount} {p.meetingCount === 1 ? "meeting" : "meetings"}
                        {" · "}
                        last {fmtDate(p.lastSeenMs)}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </section>

            {/* Notes */}
            <section className="space-y-1.5">
              <h2 className="text-xs font-medium text-muted-foreground">Notes</h2>
              <Textarea
                value={notesDraft}
                onChange={(e) => {
                  setNotesDraft(e.target.value);
                  scheduleNotesSave(e.target.value);
                }}
                placeholder="Account notes…"
                className="min-h-24 w-full resize-y rounded-lg bg-card px-3 py-3 text-sm"
              />
              <p className="text-[11px] text-muted-foreground/70">
                {notesStatus === "saving"
                  ? "Saving…"
                  : notesStatus === "modified"
                    ? "Modified"
                    : "Saved"}
              </p>
            </section>
          </div>
        </ScrollArea>

        {/* Rollup panel */}
        <aside className="hidden w-96 shrink-0 border-l lg:block">
          <CustomerSummaryPanel
            customerId={id}
            meetingCountWithSummary={customer.meetingsWithSummaryCount}
          />
        </aside>
      </div>

      {/* Merge dialog */}
      <AlertDialog open={mergeOpen} onOpenChange={(o) => !merging && setMergeOpen(o)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Merge into {customer.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              Pick a customer to merge <strong>into</strong> {customer.name}. All of that customer's
              meetings move here, and the picked customer is deleted. Personas are global and need
              no change — {customer.name}'s roster will reflect the union. This is irreversible.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <Select
            value={mergePick != null ? String(mergePick) : undefined}
            onValueChange={(v) => setMergePick(Number(v))}
          >
            <SelectTrigger className="w-full bg-card text-sm">
              <SelectValue placeholder="Select a customer…" />
            </SelectTrigger>
            <SelectContent position="popper" className="z-[60] max-h-72">
              {allCustomers.map((c) => (
                <SelectItem key={c.id} value={String(c.id)}>
                  {c.name} ({c.meetingCount} meetings)
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={merging}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={cn(buttonVariants({ variant: "destructive" }))}
              disabled={merging || mergePick == null}
              onClick={(e) => {
                e.preventDefault();
                confirmMerge();
              }}
            >
              {merging ? <Loader2 className="animate-spin" /> : <Merge />}
              Merge & delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Delete dialog */}
      <AlertDialog open={deleteOpen} onOpenChange={(o) => !deleting && setDeleteOpen(o)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete customer?</AlertDialogTitle>
            <AlertDialogDescription>
              This deletes <span className="font-medium text-foreground">{customer.name}</span>. Its
              meetings become <strong>unassigned</strong> (none are deleted) and personas are
              untouched. This cannot be undone.
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
