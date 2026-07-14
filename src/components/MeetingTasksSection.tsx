/**
 * Tasks section for the meeting page's Tasks tab. Self-contained: fetches its
 * own meeting-scoped task list (`listTasksForMeeting`), renders AI suggestions
 * (Accept/Dismiss) above accepted tasks (open first, then a collapsible
 * Completed group) via `TaskItem`, and an inline "add task from this meeting"
 * form pre-linked to the meeting + its customer.
 *
 * Extraction sources its candidate tasks from the meeting's **summary**
 * (the `## My action items` block only — what others committed to stays in
 * the summary as reference), so it **auto-runs** at the end of Generate
 * Summary (backend spawns it; this section reloads on the
 * `tasks:extraction:done` event). The summary reads naturally — plain bullets,
 * no `[priority]` tags or `due` clauses — so the parse yields a clean title
 * per item; **priority and due date are then suggested by the extraction
 * LLM** (it looks at the action items + the transcript), not parsed from the
 * summary. The manual "Extract tasks" button re-runs it (re-parses the latest
 * summary; dedupe makes it idempotent) and is disabled until a transcript
 * exists (a summary can't exist without one). Extraction is best-effort: a
 * soft error is surfaced inline; the transcript/summary are never affected.
 *
 * If the meeting has no customer, the add form shows a customer `PickerCombobox`
 * (required) — the task needs a customer; the meeting itself stays unassigned.
 * The picker can also create a new customer inline. Customer-less suggestions
 * (extracted before the meeting was assigned, for instance) default to the
 * meeting's current customer at accept time, and only require a pick when the
 * meeting itself has no customer (handled in `SuggestionItem`).
 *
 * The meeting link chip is omitted on `TaskItem` here — it's implied by the
 * page — but the customer chip is shown.
 */
import { useCallback, useEffect, useState } from "react";
import { ChevronDown, ChevronRight, ListTodo, Loader2, Plus, Sparkles } from "lucide-react";

import DueDateField from "@/components/DueDateField";
import PickerCombobox from "@/components/PickerCombobox";
import SuggestionItem from "@/components/SuggestionItem";
import TaskItem from "@/components/TaskItem";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import {
  createTask,
  extractMeetingTasks,
  listTasksForMeeting,
  onTasksExtractionDone,
  onTasksExtractionError,
  type CustomerSummary,
  type Task,
  type TaskPriority,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";
import type { Route } from "@/App";

interface Props {
  meetingId: number;
  meetingCustomerId: number | null;
  customers: CustomerSummary[];
  onCreateCustomer: (name: string) => Promise<number>;
  onNavigate: (route: Route) => void;
  /** Whether the meeting has a transcript yet — gates the Extract button. */
  hasTranscript: boolean;
  /** Jump to the transcript at a timestamp (ms). Only provided when audio is
   * available; switches the meeting page to the Transcript tab and seeks. */
  onJumpToTranscript?: (ms: number) => void;
}

export default function MeetingTasksSection({
  meetingId,
  meetingCustomerId,
  customers,
  onCreateCustomer,
  onNavigate,
  hasTranscript,
  onJumpToTranscript,
}: Props) {
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Inline add form state. `selectedCustomerId` is only used when the meeting
  // has no customer; otherwise the form is pre-linked to `meetingCustomerId`.
  const [title, setTitle] = useState("");
  const [dueAt, setDueAt] = useState<number | null>(null);
  const [priority, setPriority] = useState<TaskPriority>("normal");
  const [selectedCustomerId, setSelectedCustomerId] = useState<number | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [adding, setAdding] = useState(false);

  const [showCompleted, setShowCompleted] = useState(false);

  // Extraction state: `extracting` covers the local command call; an error
  // event from the backend (e.g. sidecar unreachable) is surfaced softly.
  const [extracting, setExtracting] = useState(false);
  const [extractError, setExtractError] = useState<string | null>(null);

  const reload = useCallback(() => {
    listTasksForMeeting(meetingId)
      .then(setTasks)
      .catch((e) => setError(String(e)));
  }, [meetingId]);

  useEffect(() => {
    reload();
  }, [reload]);

  // Soft error from the backend if a DB insert fails or there's no summary.
  // (Enrichment LLM failure is silent on the backend — tasks still insert.)
  // The wrapper unwraps the payload, so `e` is already `TasksExtractionError`.
  useTauriEvent(
    onTasksExtractionError,
    useCallback(
      (e) => {
        if (e.meetingId === meetingId) {
          setExtractError(e.message);
          setExtracting(false);
        }
      },
      [meetingId],
    ),
  );

  // Reload when extraction completes — this is what makes the Tasks tab pick
  // up suggestions after the **auto-trigger** fires `done` at the end of
  // Generate Summary (which happens on the Review tab while this section is
  // hidden-but-mounted). Also covers the manual button's backend `done`.
  useTauriEvent(
    onTasksExtractionDone,
    useCallback(
      (e) => {
        if (e.meetingId === meetingId) {
          setExtracting(false);
          reload();
        }
      },
      [meetingId, reload],
    ),
  );

  async function extract() {
    setExtractError(null);
    setExtracting(true);
    try {
      await extractMeetingTasks(meetingId);
      reload();
    } catch (e) {
      setExtractError(String(e));
    } finally {
      setExtracting(false);
    }
  }

  const customerName = (id: number | null) =>
    id != null ? customers.find((c) => c.id === id)?.name : undefined;

  // The customer the add form will attach the task to.
  const formCustomerId = meetingCustomerId ?? selectedCustomerId;
  const needsCustomerPicker = meetingCustomerId == null;

  async function add() {
    const t = title.trim();
    if (!t || formCustomerId == null) return;
    setAdding(true);
    try {
      await createTask({
        customerId: formCustomerId,
        title: t,
        priority,
        dueAt,
        sourceMeetingId: meetingId,
      });
      setTitle("");
      setDueAt(null);
      setPriority("normal");
      // Keep `selectedCustomerId` so adding several tasks to the same
      // customer-less meeting is rapid-fire; clear it only on explicit unassign.
      reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setAdding(false);
    }
  }

  const suggestions = tasks?.filter((t) => t.status === "suggested") ?? [];
  const open = tasks?.filter((t) => t.status === "open") ?? [];
  const done = tasks?.filter((t) => t.status === "done") ?? [];
  const loaded = tasks != null;

  return (
    <section className="mx-auto max-w-3xl space-y-4 p-6 pt-4">
      <div className="flex items-center justify-between gap-2">
        <h1 className="flex items-center gap-1.5 text-lg font-semibold tracking-tight">
          <ListTodo className="size-4 text-muted-foreground" /> Tasks
        </h1>
        <Button
          size="sm"
          variant="outline"
          className="h-8"
          disabled={!hasTranscript || extracting}
          onClick={extract}
          title={
            hasTranscript
              ? "Re-extract action items from the latest summary"
              : "Transcribe first to extract tasks"
          }
        >
          {extracting ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <Sparkles className="size-4" />
          )}
          Extract tasks
        </Button>
      </div>

      {extractError && <p className="text-xs text-destructive">{extractError}</p>}

      {/* Inline add form. The title input is the flexible element; the
       * customer picker (only when the meeting has no customer), date,
       * priority, and Add controls are grouped as non-wrapping units so the
       * Add button can never get orphaned on its own line. */}
      <div className="rounded-lg border bg-card p-2.5">
        <div className="flex flex-wrap items-center gap-2">
          {needsCustomerPicker ? (
            <PickerCombobox
              open={pickerOpen}
              onOpenChange={setPickerOpen}
              trigger={
                <button
                  type="button"
                  className="flex h-8 w-44 shrink-0 items-center justify-between gap-2 rounded-md border border-input bg-background px-2.5 text-sm whitespace-nowrap shadow-xs transition-[color,box-shadow] outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 data-[placeholder]:text-muted-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 [&_svg:not([class*='text-'])]:text-muted-foreground"
                >
                  <span
                    className="truncate data-[placeholder]:text-muted-foreground"
                    data-placeholder={selectedCustomerId != null ? undefined : ""}
                  >
                    {customerName(selectedCustomerId) ?? "Pick customer"}
                  </span>
                  <ChevronDown className="size-4 opacity-50" />
                </button>
              }
              items={customers.map((c) => ({
                id: c.id,
                label: c.name,
                sublabel: `${c.meetingCount} mtgs`,
              }))}
              currentId={selectedCustomerId}
              currentLabel={customerName(selectedCustomerId)}
              currentSublabel={
                selectedCustomerId != null
                  ? `${customers.find((c) => c.id === selectedCustomerId)?.meetingCount ?? 0} mtgs`
                  : undefined
              }
              onPick={setSelectedCustomerId}
              onCreate={async (name) => {
                const cid = await onCreateCustomer(name);
                setSelectedCustomerId(cid);
                return cid;
              }}
              onUnassign={() => setSelectedCustomerId(null)}
              unassignLabel="Clear customer"
              createNoun="customer"
              placeholder="Search customers…"
            />
          ) : (
            <span className="inline-flex h-8 shrink-0 items-center rounded-md bg-secondary px-2.5 text-xs text-muted-foreground">
              for {customerName(meetingCustomerId) ?? "this customer"}
            </span>
          )}

          <Input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && title.trim() && formCustomerId != null) add();
            }}
            placeholder="Add a task from this meeting…"
            className="h-8 min-w-40 flex-1 rounded-md bg-background text-sm"
          />
          <div className="flex items-center gap-2">
            <DueDateField value={dueAt} onChange={setDueAt} ariaLabel="Due date" />
            <Select value={priority} onValueChange={(v) => setPriority(v as TaskPriority)}>
              <SelectTrigger className="h-8 min-w-24 rounded-md bg-background text-sm text-muted-foreground">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="low">Low</SelectItem>
                <SelectItem value="normal">Normal</SelectItem>
                <SelectItem value="high">High</SelectItem>
              </SelectContent>
            </Select>
            <Button
              size="sm"
              className="h-8 shrink-0"
              disabled={adding || !title.trim() || formCustomerId == null}
              onClick={add}
            >
              <Plus className="size-4" /> Add
            </Button>
          </div>
        </div>
        {needsCustomerPicker && (
          <p className="mt-1.5 px-1 text-[11px] text-muted-foreground/70">
            This meeting has no customer — pick one to attach the task to.
          </p>
        )}
      </div>

      {error && <p className="text-xs text-destructive">{error}</p>}

      {/* Pending AI suggestions: Accept promotes to an open task. A suggestion
       * with no customer defaults to the meeting's customer; only when the
       * meeting is also customer-less is a customer picked inline in
       * SuggestionItem. Dismiss hides a suggestion; re-extracting (this button
       * or a summary re-generate) revives it if its action item is still in the
       * summary. */}
      {loaded && suggestions.length > 0 && (
        <div className="rounded-lg border bg-card">
          <div className="flex items-center gap-1.5 border-b p-2.5 text-xs font-medium text-muted-foreground">
            <Sparkles className="size-3.5" />
            Suggestions ({suggestions.length})
          </div>
          <div className="divide-y">
            {suggestions.map((s) => (
              <SuggestionItem
                key={s.id}
                suggestion={s}
                customers={customers}
                onCreateCustomer={onCreateCustomer}
                onReload={reload}
                onNavigate={onNavigate}
                onJumpToTranscript={onJumpToTranscript}
                meetingCustomerId={meetingCustomerId}
              />
            ))}
          </div>
        </div>
      )}

      {!loaded ? (
        <p className="rounded-lg border bg-card p-3 text-xs text-muted-foreground">
          Loading tasks…
        </p>
      ) : open.length === 0 && done.length === 0 ? (
        <p className="rounded-lg border bg-card p-8 text-center text-xs text-muted-foreground">
          {suggestions.length > 0
            ? "Accept the suggestions above, or add a task manually."
            : "No tasks for this meeting yet. Add one above, or click Extract tasks."}
        </p>
      ) : (
        <div className="space-y-3">
          {open.length > 0 && (
            <div className="divide-y rounded-lg border bg-card">
              {open.map((t) => (
                <TaskItem
                  key={t.id}
                  task={t}
                  onReload={reload}
                  onNavigate={onNavigate}
                  customerName={customerName(t.customerId)}
                />
              ))}
            </div>
          )}

          {done.length > 0 && (
            <div className="rounded-lg border bg-card">
              <button
                onClick={() => setShowCompleted((s) => !s)}
                className="flex w-full items-center gap-1.5 p-2.5 text-xs font-medium text-muted-foreground transition-colors hover:bg-accent/50"
              >
                <ChevronRight
                  className={cn("size-3.5 transition-transform", showCompleted && "rotate-90")}
                />
                Completed ({done.length})
              </button>
              {showCompleted && (
                <div className="divide-y border-t">
                  {done.map((t) => (
                    <TaskItem
                      key={t.id}
                      task={t}
                      onReload={reload}
                      onNavigate={onNavigate}
                      customerName={customerName(t.customerId)}
                    />
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}
