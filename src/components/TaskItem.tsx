/**
 * A single task row, reused by the customer page, global Tasks view, and
 * meeting page. Mutations (complete/uncomplete, delete) call IPC then the
 * parent's `onReload` so the owning surface refetches its scoped list.
 *
 * Props select which metadata to show: the customer page hides the customer
 * link (it's implied); the meeting page hides the meeting link; the global
 * view shows both. Suggestions (status === "suggested") are rendered by the
 * meeting page with Accept/Dismiss controls, not by this component — this
 * component only handles real (open/done) tasks.
 */
import { useState } from "react";
import { Loader2, Trash2 } from "lucide-react";

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
import { Checkbox } from "@/components/ui/checkbox";
import { cn } from "@/lib/utils";
import { deleteTask, setTaskStatus, type Task, type TaskPriority } from "@/lib/ipc";
import { fmtDueDate } from "@/lib/dates";
import type { Route } from "@/App";

interface Props {
  task: Task;
  onReload: () => void;
  onNavigate?: (route: Route) => void;
  /** Show the linked meeting's title as a clickable chip (global view). */
  meetingTitle?: string;
  /** Show the linked customer's name as a clickable chip (global view). */
  customerName?: string;
}

const PRIORITY_PILL: Record<TaskPriority, string> = {
  high: "bg-destructive/15 text-destructive",
  normal: "bg-secondary text-muted-foreground",
  low: "bg-secondary text-muted-foreground/60",
};

const PRIORITY_LABEL: Record<TaskPriority, string> = {
  high: "High",
  normal: "Normal",
  low: "Low",
};

export default function TaskItem({
  task,
  onReload,
  onNavigate,
  meetingTitle,
  customerName,
}: Props) {
  const [toggling, setToggling] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const done = task.status === "done";
  const due = fmtDueDate(task.dueAt);

  async function toggle() {
    setToggling(true);
    try {
      await setTaskStatus(task.id, done ? "open" : "done");
      onReload();
    } finally {
      setToggling(false);
    }
  }

  async function confirmDelete() {
    setDeleting(true);
    try {
      await deleteTask(task.id);
      setDeleteOpen(false);
      onReload();
    } finally {
      setDeleting(false);
    }
  }

  return (
    <div className="group flex items-start gap-3 p-3">
      <div className="pt-0.5">
        <Checkbox
          checked={done}
          onCheckedChange={toggle}
          disabled={toggling}
          aria-label={done ? "Mark task as open" : "Mark task as done"}
        />
      </div>

      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex items-start justify-between gap-2">
          <span
            className={cn(
              "text-sm font-medium leading-snug",
              done && "text-muted-foreground line-through",
            )}
          >
            {task.title}
          </span>
          <Button
            variant="ghost"
            size="icon"
            className="size-7 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100"
            onClick={() => setDeleteOpen(true)}
            aria-label="Delete task"
          >
            <Trash2 className="size-4" />
          </Button>
        </div>

        {task.description && (
          <p
            className={cn(
              "text-xs leading-snug",
              done ? "text-muted-foreground/60" : "text-muted-foreground",
            )}
          >
            {task.description}
          </p>
        )}

        <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
          {/* Due date */}
          <span
            className={cn(
              "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
              due.overdue && !done
                ? "bg-destructive/15 text-destructive"
                : "bg-secondary text-muted-foreground",
              done && "text-muted-foreground/60",
            )}
          >
            {due.label}
          </span>

          {/* Priority (only when not "normal", to reduce visual noise) */}
          {task.priority !== "normal" && (
            <span
              className={cn(
                "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
                PRIORITY_PILL[task.priority],
                done && "opacity-60",
              )}
            >
              {PRIORITY_LABEL[task.priority]}
            </span>
          )}

          {/* AI origin badge */}
          {task.origin === "ai" && (
            <span className="inline-flex items-center rounded-full bg-secondary px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
              AI
            </span>
          )}

          {/* Customer link (global view) */}
          {customerName && task.customerId != null && onNavigate && (
            <button
              onClick={() => onNavigate({ name: "customer", customerId: String(task.customerId) })}
              className="inline-flex items-center rounded-full bg-secondary px-2 py-0.5 text-[11px] font-medium text-muted-foreground transition-colors hover:text-foreground"
            >
              {customerName}
            </button>
          )}

          {/* Meeting link (global view) */}
          {meetingTitle && task.sourceMeetingId != null && onNavigate && (
            <button
              onClick={() =>
                onNavigate({ name: "meeting", meetingId: String(task.sourceMeetingId) })
              }
              className="inline-flex max-w-40 items-center truncate rounded-full bg-secondary px-2 py-0.5 text-[11px] font-medium text-muted-foreground transition-colors hover:text-foreground"
            >
              {meetingTitle}
            </button>
          )}
        </div>
      </div>

      <AlertDialog
        open={deleteOpen}
        onOpenChange={(open) => {
          if (!open && !deleting) setDeleteOpen(open);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete task?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently deletes{" "}
              <span className="font-medium text-foreground">{task.title}</span>. This cannot be
              undone.
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
