/**
 * Tasks section for the customer detail page. Self-contained: fetches its own
 * scoped task list (`listTasksForCustomer`), renders open tasks first with a
 * collapsible "Completed" group, and an inline add form pre-linked to the
 * customer. Reloads after every mutation.
 *
 * Scoping is enforced server-side (the command only returns this customer's
 * tasks); this component just renders what it gets back.
 */
import { useCallback, useEffect, useState } from "react";
import { ChevronRight, ListTodo, Plus } from "lucide-react";

import DueDateField from "@/components/DueDateField";
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
import { createTask, listTasksForCustomer, type Task, type TaskPriority } from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  customerId: number;
  onNavigate: (route: Route) => void;
}

export default function CustomerTasksSection({ customerId, onNavigate }: Props) {
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Inline add form state.
  const [title, setTitle] = useState("");
  const [dueAt, setDueAt] = useState<number | null>(null);
  const [priority, setPriority] = useState<TaskPriority>("normal");
  const [adding, setAdding] = useState(false);

  const [showCompleted, setShowCompleted] = useState(false);

  const reload = useCallback(() => {
    listTasksForCustomer(customerId)
      .then(setTasks)
      .catch((e) => setError(String(e)));
  }, [customerId]);

  useEffect(() => {
    reload();
  }, [reload]);

  async function add() {
    const t = title.trim();
    if (!t) return;
    setAdding(true);
    try {
      await createTask({
        customerId,
        title: t,
        priority,
        dueAt,
      });
      setTitle("");
      setDueAt(null);
      setPriority("normal");
      reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setAdding(false);
    }
  }

  const open = tasks?.filter((t) => t.status === "open") ?? [];
  const done = tasks?.filter((t) => t.status === "done") ?? [];
  const loaded = tasks != null;

  return (
    <section className="space-y-1.5">
      <h2 className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        <ListTodo className="size-3.5" /> Tasks ({open.length} open)
      </h2>

      {/* Inline add form. The title input is the flexible element; the
       * date + priority + Add controls are grouped as a single non-wrapping
       * unit so the Add button can never get orphaned on its own line when
       * the date label grows ("Tomorrow" / "Overdue · Jul 14"). On narrow
       * widths the whole controls group wraps together below the title. */}
      <div className="rounded-lg border bg-card p-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <Input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && title.trim()) add();
            }}
            placeholder="Add a task…"
            className="h-8 min-w-40 flex-1 rounded-md bg-background text-sm"
          />
          <div className="flex items-center gap-2">
            <DueDateField value={dueAt} onChange={setDueAt} ariaLabel="Due date" />
            <Select value={priority} onValueChange={(v) => setPriority(v as TaskPriority)}>
              <SelectTrigger className="h-8 w-24 shrink-0 rounded-md bg-background text-sm text-muted-foreground">
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
              disabled={adding || !title.trim()}
              onClick={add}
            >
              <Plus className="size-4" /> Add
            </Button>
          </div>
        </div>
      </div>

      {error && <p className="text-xs text-destructive">{error}</p>}

      {!loaded ? (
        <p className="rounded-lg border bg-card p-3 text-xs text-muted-foreground">
          Loading tasks…
        </p>
      ) : open.length === 0 && done.length === 0 ? (
        <p className="rounded-lg border bg-card p-8 text-center text-xs text-muted-foreground">
          No tasks yet. Add one above.
        </p>
      ) : (
        <div className="space-y-3">
          {open.length > 0 && (
            <div className="divide-y rounded-lg border bg-card">
              {open.map((t) => (
                <TaskItem key={t.id} task={t} onReload={reload} onNavigate={onNavigate} />
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
                    <TaskItem key={t.id} task={t} onReload={reload} onNavigate={onNavigate} />
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
