/**
 * Global Tasks / daily-planning view. Shows every task across all customers
 * (suggestions excluded — those live on the meeting page). Open tasks are
 * grouped by time bucket (Overdue → Today → Upcoming → No due date), or by
 * customer when the "Group by customer" toggle is on. Completed tasks sit in a
 * collapsible "Completed" section, hidden unless "Show completed" is on.
 *
 * Filtering (customer / priority / origin) and the show-completed / group
 * toggles are all client-side over a single `listAllTasks` fetch — the working
 * set is small (local meeting app) and this keeps filter changes instant
 * without a round-trip. Only the sort key triggers a refetch (its null-handling
 * / priority ranking is cleaner in SQL).
 *
 * Each row reuses `TaskItem`, which shows both the customer and meeting link
 * chips here (the customer page hides the customer link; the meeting page hides
 * the meeting link).
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { ChevronRight, ListTodo } from "lucide-react";

import TaskItem from "@/components/TaskItem";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import { startOfTodayMs } from "@/lib/dates";
import {
  listAllTasks,
  listCustomers,
  listMeetings,
  type CustomerSummary,
  type MeetingSummary,
  type Task,
  type TaskOrigin,
  type TaskPriority,
  type TaskSort,
} from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  onNavigate: (route: Route) => void;
}

const DAY_MS = 86_400_000;

function atMidnight(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Day offset of a due date vs today (negative = past, 0 = today). */
function dayDiff(dueAt: number): number {
  return Math.round((atMidnight(dueAt) - startOfTodayMs()) / DAY_MS);
}

type Bucket = "overdue" | "today" | "upcoming" | "nodue";

const BUCKET_LABEL: Record<Bucket, string> = {
  overdue: "Overdue",
  today: "Today",
  upcoming: "Upcoming",
  nodue: "No due date",
};

const BUCKET_ORDER: Bucket[] = ["overdue", "today", "upcoming", "nodue"];

function bucketOf(task: Task): Bucket {
  if (task.dueAt == null) return "nodue";
  return dayDiff(task.dueAt) < 0 ? "overdue" : dayDiff(task.dueAt) === 0 ? "today" : "upcoming";
}

export default function TasksView({ onNavigate }: Props) {
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const [customers, setCustomers] = useState<CustomerSummary[]>([]);
  const [meetings, setMeetings] = useState<MeetingSummary[]>([]);
  const [error, setError] = useState<string | null>(null);

  // Filters (client-side). `sort` is the only one that triggers a refetch.
  const [customerFilter, setCustomerFilter] = useState<number | null>(null);
  const [priorityFilter, setPriorityFilter] = useState<TaskPriority | null>(null);
  const [originFilter, setOriginFilter] = useState<TaskOrigin | null>(null);
  const [sort, setSort] = useState<TaskSort>("due");
  const [showCompleted, setShowCompleted] = useState(false);
  const [groupByCustomer, setGroupByCustomer] = useState(false);

  // Collapsible "Completed" section (collapsed by default).
  const [completedOpen, setCompletedOpen] = useState(false);

  const reload = useCallback(() => {
    Promise.all([listAllTasks(sort), listCustomers(), listMeetings()])
      .then(([t, c, m]) => {
        setTasks(t);
        setCustomers(c);
        setMeetings(m);
      })
      .catch((e) => {
        setError(String(e));
        setTasks([]);
      });
  }, [sort]);

  useEffect(() => {
    reload();
  }, [reload]);

  const customerName = useCallback(
    (id: number | null) => (id != null ? customers.find((c) => c.id === id)?.name : undefined),
    [customers],
  );
  const meetingTitle = useCallback(
    (id: number | null) => (id != null ? meetings.find((m) => m.id === id)?.title : undefined),
    [meetings],
  );

  // Apply client-side filters.
  const { open, done } = useMemo(() => {
    if (!tasks) return { open: [] as Task[], done: [] as Task[] };
    const matches = (t: Task) =>
      (customerFilter == null || t.customerId === customerFilter) &&
      (priorityFilter == null || t.priority === priorityFilter) &&
      (originFilter == null || t.origin === originFilter);
    return {
      open: tasks.filter((t) => t.status === "open" && matches(t)),
      done: tasks.filter((t) => t.status === "done" && matches(t)),
    };
  }, [tasks, customerFilter, priorityFilter, originFilter]);

  // Time-bucketed open tasks (default grouping).
  const buckets = useMemo(() => {
    const map: Record<Bucket, Task[]> = { overdue: [], today: [], upcoming: [], nodue: [] };
    for (const t of open) map[bucketOf(t)].push(t);
    return map;
  }, [open]);

  // Customer-grouped open tasks.
  const byCustomer = useMemo(() => {
    const map = new Map<string, { label: string; tasks: Task[] }>();
    for (const t of open) {
      const key = t.customerId != null ? String(t.customerId) : "__none__";
      const label =
        t.customerId != null ? (customerName(t.customerId) ?? "Unknown customer") : "No customer";
      const entry = map.get(key) ?? { label, tasks: [] };
      if (!map.has(key)) map.set(key, entry);
      entry.tasks.push(t);
    }
    // Sort group headers alphabetically, "No customer" last.
    return [...map.values()].sort((a, b) => {
      if (a.label === "No customer") return 1;
      if (b.label === "No customer") return -1;
      return a.label.localeCompare(b.label);
    });
  }, [open, customerName]);

  const hasAnyTasks = (tasks?.length ?? 0) > 0;
  const activeFilter = customerFilter != null || priorityFilter != null || originFilter != null;

  function clearFilters() {
    setCustomerFilter(null);
    setPriorityFilter(null);
    setOriginFilter(null);
  }

  // Full-page empty state: no tasks exist at all.
  if (tasks != null && !hasAnyTasks && !activeFilter) {
    return (
      <div className="mx-auto flex h-full max-w-2xl flex-col items-center justify-center gap-6 p-8 text-center">
        <div className="flex size-14 items-center justify-center rounded-2xl bg-secondary">
          <ListTodo className="size-6 text-muted-foreground" />
        </div>
        <div className="space-y-1.5">
          <h1 className="text-xl font-semibold tracking-tight">No tasks yet</h1>
          <p className="text-sm text-muted-foreground">
            Add tasks from a customer page or a meeting's Tasks tab. They'll all show up here for
            daily planning.
          </p>
        </div>
        <Button onClick={() => onNavigate({ name: "customers" })}>Browse customers</Button>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-3xl space-y-4 p-8 pt-4">
      <div className="flex items-center justify-between gap-4">
        <h1 className="text-lg font-semibold tracking-tight">Tasks</h1>
        <div className="flex items-center gap-4 text-xs text-muted-foreground">
          <label className="flex items-center gap-1.5">
            <Checkbox
              checked={groupByCustomer}
              onCheckedChange={setGroupByCustomer}
              aria-label="Group by customer"
            />
            Group by customer
          </label>
          <label className="flex items-center gap-1.5">
            <Checkbox
              checked={showCompleted}
              onCheckedChange={setShowCompleted}
              aria-label="Show completed tasks"
            />
            Show completed
          </label>
        </div>
      </div>

      {/* Filter / sort bar. Each Select uses an "all" sentinel value meaning
       * "any" so the value is always a non-empty string (shadcn Select needs
       * one). */}
      <div className="flex flex-wrap items-center gap-2">
        <Select
          value={customerFilter != null ? String(customerFilter) : "all"}
          onValueChange={(v) => setCustomerFilter(v === "all" ? null : Number(v))}
        >
          <SelectTrigger className="h-8 min-w-40 rounded-md bg-card text-sm text-muted-foreground">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All customers</SelectItem>
            {customers.map((c) => (
              <SelectItem key={c.id} value={String(c.id)}>
                {c.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <Select
          value={priorityFilter ?? "all"}
          onValueChange={(v) => setPriorityFilter(v === "all" ? null : (v as TaskPriority))}
        >
          <SelectTrigger className="h-8 min-w-32 rounded-md bg-card text-sm text-muted-foreground">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">Any priority</SelectItem>
            <SelectItem value="high">High</SelectItem>
            <SelectItem value="normal">Normal</SelectItem>
            <SelectItem value="low">Low</SelectItem>
          </SelectContent>
        </Select>

        <Select
          value={originFilter ?? "all"}
          onValueChange={(v) => setOriginFilter(v === "all" ? null : (v as TaskOrigin))}
        >
          <SelectTrigger className="h-8 min-w-32 rounded-md bg-card text-sm text-muted-foreground">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">Any origin</SelectItem>
            <SelectItem value="manual">Manual</SelectItem>
            <SelectItem value="ai">AI</SelectItem>
          </SelectContent>
        </Select>

        <Select value={sort} onValueChange={(v) => setSort(v as TaskSort)}>
          <SelectTrigger className="h-8 min-w-40 rounded-md bg-card text-sm text-muted-foreground">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="due">Sort: Due date</SelectItem>
            <SelectItem value="priority">Sort: Priority</SelectItem>
            <SelectItem value="created">Sort: Created</SelectItem>
          </SelectContent>
        </Select>

        {activeFilter && (
          <Button variant="ghost" size="sm" className="h-8" onClick={clearFilters}>
            Clear filters
          </Button>
        )}
      </div>

      {error && <p className="text-sm text-destructive">{error}</p>}

      {tasks == null ? (
        <p className="py-8 text-center text-sm text-muted-foreground">Loading tasks…</p>
      ) : open.length === 0 && done.length === 0 ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
          {activeFilter ? "No tasks match these filters." : "Nothing to do 🎉 All caught up."}
        </p>
      ) : (
        <div className="space-y-6">
          {/* Open tasks */}
          {open.length === 0 ? (
            done.length > 0 && !showCompleted ? (
              <p className="py-8 text-center text-sm text-muted-foreground">
                Nothing to do 🎉 All caught up. Toggle “Show completed” to see finished tasks.
              </p>
            ) : null
          ) : groupByCustomer ? (
            <div className="space-y-4">
              {byCustomer.map((group) => (
                <div key={group.label} className="space-y-1.5">
                  <h2 className="text-xs font-medium text-muted-foreground">{group.label}</h2>
                  <div className="divide-y rounded-lg border bg-card">
                    {group.tasks.map((t) => (
                      <TaskItem
                        key={t.id}
                        task={t}
                        onReload={reload}
                        onNavigate={onNavigate}
                        customerName={customerName(t.customerId)}
                        meetingTitle={meetingTitle(t.sourceMeetingId)}
                      />
                    ))}
                  </div>
                </div>
              ))}
            </div>
          ) : (
            <div className="space-y-4">
              {BUCKET_ORDER.map((b) =>
                buckets[b].length === 0 ? null : (
                  <div key={b} className="space-y-1.5">
                    <h2
                      className={cn(
                        "text-xs font-medium",
                        b === "overdue" ? "text-destructive" : "text-muted-foreground",
                      )}
                    >
                      {BUCKET_LABEL[b]} ({buckets[b].length})
                    </h2>
                    <div className="divide-y rounded-lg border bg-card">
                      {buckets[b].map((t) => (
                        <TaskItem
                          key={t.id}
                          task={t}
                          onReload={reload}
                          onNavigate={onNavigate}
                          customerName={customerName(t.customerId)}
                          meetingTitle={meetingTitle(t.sourceMeetingId)}
                        />
                      ))}
                    </div>
                  </div>
                ),
              )}
            </div>
          )}

          {/* Completed (only when "Show completed" is on) */}
          {showCompleted && done.length > 0 && (
            <div className="space-y-1.5">
              <button
                onClick={() => setCompletedOpen((s) => !s)}
                className="flex w-full items-center gap-1.5 rounded-lg border bg-card p-2.5 text-xs font-medium text-muted-foreground transition-colors hover:bg-accent/50"
              >
                <ChevronRight
                  className={cn("size-3.5 transition-transform", completedOpen && "rotate-90")}
                />
                Completed ({done.length})
              </button>
              {completedOpen && (
                <div className="divide-y rounded-lg border bg-card">
                  {done.map((t) => (
                    <TaskItem
                      key={t.id}
                      task={t}
                      onReload={reload}
                      onNavigate={onNavigate}
                      customerName={customerName(t.customerId)}
                      meetingTitle={meetingTitle(t.sourceMeetingId)}
                    />
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
