/**
 * A pending AI task suggestion (status === "suggested") on the meeting page's
 * Tasks tab. Shows the extracted title + supporting snippet/timestamp, with
 * Accept and Dismiss controls.
 *
 * A suggestion's customer is stamped at extraction time from the meeting's
 * customer then. If the meeting had no customer at that moment (e.g. the
 * customer was assigned after Generate Summary auto-triggered extraction),
 * the suggestion row is customer-less — so we fall back to the meeting's
 * *current* customer (`meetingCustomerId`) as the default. Only when both are
 * null does Accept require picking a customer first via a `PickerCombobox`
 * (which can also create a new customer inline).
 *
 * Accept → `acceptTaskSuggestion` (promotes to an `open` task with the
 * customer). Dismiss → `dismissTaskSuggestion` (hides it; re-extracting
 * revives it as a suggestion if the action item is still in the summary).
 * Both call the parent's `onReload`.
 */
import { useState } from "react";
import { ChevronDown, Check, Loader2, Sparkles, X } from "lucide-react";

import PickerCombobox from "@/components/PickerCombobox";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import {
  acceptTaskSuggestion,
  dismissTaskSuggestion,
  type CustomerSummary,
  type Task,
  type TaskPriority,
} from "@/lib/ipc";
import { fmtDueDate } from "@/lib/dates";
import type { Route } from "@/App";

interface Props {
  suggestion: Task;
  customers: CustomerSummary[];
  onCreateCustomer: (name: string) => Promise<number>;
  onReload: () => void;
  onNavigate?: (route: Route) => void;
  /** Jump to the transcript at a timestamp (ms) — switches the meeting page to
   * the Transcript tab and seeks the audio player if audio exists. */
  onJumpToTranscript?: (ms: number) => void;
  /** The meeting's current customer — used as the default when the suggestion
   * itself has none (e.g. extracted before the meeting was assigned). */
  meetingCustomerId: number | null;
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

function fmtMs(ms: number): string {
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export default function SuggestionItem({
  suggestion,
  customers,
  onCreateCustomer,
  onReload,
  onJumpToTranscript,
  meetingCustomerId,
}: Props) {
  const [accepting, setAccepting] = useState(false);
  const [dismissing, setDismissing] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  // Customer chosen for a customer-less suggestion (required to accept).
  const [selectedCustomerId, setSelectedCustomerId] = useState<number | null>(null);

  const due = fmtDueDate(suggestion.dueAt);
  // The suggestion's own customer wins; fall back to the meeting's current
  // customer when the suggestion has none (e.g. extracted before the meeting
  // was assigned). Only when both are null does Accept require a pick.
  const effectiveCustomerId = suggestion.customerId ?? meetingCustomerId;
  const hasCustomer = effectiveCustomerId != null;
  const acceptCustomerId = effectiveCustomerId ?? selectedCustomerId;

  const customerName = (id: number | null) =>
    id != null ? customers.find((c) => c.id === id)?.name : undefined;

  async function accept() {
    if (acceptCustomerId == null) return;
    setAccepting(true);
    try {
      await acceptTaskSuggestion(suggestion.id, acceptCustomerId);
      onReload();
    } finally {
      setAccepting(false);
    }
  }

  async function dismiss() {
    setDismissing(true);
    try {
      await dismissTaskSuggestion(suggestion.id);
      onReload();
    } finally {
      setDismissing(false);
    }
  }

  return (
    <div className="group flex items-start gap-3 p-3">
      <div className="mt-0.5 shrink-0">
        <Sparkles className="size-4 text-muted-foreground/70" />
      </div>

      <div className="min-w-0 flex-1 space-y-1">
        <span className="text-sm font-medium leading-snug">{suggestion.title}</span>

        {suggestion.description && (
          <p className="text-xs leading-snug text-muted-foreground">{suggestion.description}</p>
        )}

        {/* Transcript link: the timestamp chip jumps to where the action was
         * discussed. Shown whenever we have a timestamp — independent of the
         * supporting snippet, so the link is always present for extracted
         * tasks (the enrichment prompt always returns startMs). The snippet,
         * if any, is shown alongside it. */}
        {((suggestion.sourceStartMs != null && onJumpToTranscript) ||
          suggestion.snippet) && (
          <div className="flex items-start gap-1.5 text-xs text-muted-foreground/80">
            {suggestion.sourceStartMs != null && onJumpToTranscript && (
              <button
                onClick={() => onJumpToTranscript(suggestion.sourceStartMs!)}
                className="shrink-0 rounded-md bg-secondary px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground transition-colors hover:text-foreground"
                title="Jump to transcript"
              >
                {fmtMs(suggestion.sourceStartMs)}
              </button>
            )}
            {suggestion.snippet && (
              <span className="border-l-2 border-border pl-2 italic">
                “{suggestion.snippet}”
              </span>
            )}
          </div>
        )}

        <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
          <span className="inline-flex items-center rounded-full bg-secondary px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
            Suggestion
          </span>
          {/* Due date (only if set) */}
          {suggestion.dueAt != null && (
            <span
              className={cn(
                "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
                due.overdue
                  ? "bg-destructive/15 text-destructive"
                  : "bg-secondary text-muted-foreground",
              )}
            >
              {due.label}
            </span>
          )}
          {suggestion.priority !== "normal" && (
            <span
              className={cn(
                "inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium",
                PRIORITY_PILL[suggestion.priority],
              )}
            >
              {PRIORITY_LABEL[suggestion.priority]}
            </span>
          )}
          {hasCustomer && (
            <span className="inline-flex items-center rounded-full bg-secondary px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
              {customerName(effectiveCustomerId)}
            </span>
          )}
        </div>

        {/* Accept / Dismiss controls. */}
        <div className="flex flex-wrap items-center gap-2 pt-1.5">
          {hasCustomer ? (
            <Button size="sm" className="h-7" disabled={accepting} onClick={accept}>
              {accepting ? (
                <Loader2 className="size-3.5 animate-spin" />
              ) : (
                <Check className="size-3.5" />
              )}
              Accept
            </Button>
          ) : (
            <div className="flex items-center gap-2">
              <PickerCombobox
                open={pickerOpen}
                onOpenChange={setPickerOpen}
                trigger={
                  <button
                    type="button"
                    className="flex h-7 w-40 shrink-0 items-center justify-between gap-1.5 rounded-md border border-input bg-background px-2 text-xs whitespace-nowrap shadow-xs transition-[color,box-shadow] outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 data-[placeholder]:text-muted-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-3.5 [&_svg:not([class*='text-'])]:text-muted-foreground"
                  >
                    <span
                      className="truncate data-[placeholder]:text-muted-foreground"
                      data-placeholder={selectedCustomerId != null ? undefined : ""}
                    >
                      {customerName(selectedCustomerId) ?? "Pick customer"}
                    </span>
                    <ChevronDown className="size-3.5 opacity-50" />
                  </button>
                }
                items={customers.map((c) => ({
                  id: c.id,
                  label: c.name,
                  sublabel: `${c.meetingCount} mtgs`,
                }))}
                currentId={selectedCustomerId}
                currentLabel={customerName(selectedCustomerId)}
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
              <Button
                size="sm"
                className="h-7"
                disabled={accepting || acceptCustomerId == null}
                onClick={accept}
              >
                {accepting ? (
                  <Loader2 className="size-3.5 animate-spin" />
                ) : (
                  <Check className="size-3.5" />
                )}
                Accept
              </Button>
            </div>
          )}

          <Button
            size="sm"
            variant="ghost"
            className="h-7 text-muted-foreground"
            disabled={dismissing}
            onClick={dismiss}
          >
            {dismissing ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <X className="size-3.5" />
            )}
            Dismiss
          </Button>
        </div>
      </div>
    </div>
  );
}
