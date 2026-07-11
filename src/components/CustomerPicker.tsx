import { useEffect, useRef, useState } from "react";
import { Check, Plus, Unlink, X } from "lucide-react";

import type { CustomerSummary } from "@/lib/ipc";

interface Props {
  current: CustomerSummary | null;
  /** All existing customers for the "choose" list. */
  customers: CustomerSummary[];
  onAssign: (customerId: number | null) => void;
  /** Create a new customer and return its id. */
  onCreateCustomer: (name: string) => Promise<number>;
  /** Close the popover without side effects. */
  onDismiss: () => void;
}

/**
 * Picker for assigning / creating / unassigning a customer for a meeting.
 * Mirrors the SpeakerPersonaPicker create-new UX: list + "Create new…" toggle
 * to a free-text input (Enter creates+assigns, Esc cancels, blur commits).
 */
export default function CustomerPicker({
  current,
  customers,
  onAssign,
  onCreateCustomer,
  onDismiss,
}: Props) {
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (creating) inputRef.current?.focus();
  }, [creating]);

  const commitCreate = async () => {
    const name = draft.trim();
    if (!name) {
      setCreating(false);
      return;
    }
    try {
      const id = await onCreateCustomer(name);
      onAssign(id);
      onDismiss();
    } catch {
      setCreating(false);
    }
  };

  return (
    <div
      className="absolute z-50 w-64 rounded-lg border bg-popover p-2 shadow-md"
      onClick={(e) => e.stopPropagation()}
    >
      <div className="mb-1 flex items-center justify-between">
        <span className="text-[11px] font-medium text-muted-foreground">
          {current ? "Reassign customer" : "Assign customer"}
        </span>
        <button
          className="text-muted-foreground hover:text-foreground"
          onClick={onDismiss}
          aria-label="Dismiss"
        >
          <X className="size-3.5" />
        </button>
      </div>

      {current && (
        <button
          onClick={() => {
            onAssign(current.id);
            onDismiss();
          }}
          className="mb-1 flex w-full items-center gap-1.5 rounded-md bg-primary/10 px-2 py-1.5 text-xs font-medium hover:bg-primary/15"
        >
          <Check className="size-3.5" /> {current.name}
        </button>
      )}

      {!creating && (
        <>
          <div className="max-h-40 overflow-y-auto">
            {customers
              .filter((c) => c.id !== current?.id)
              .map((c) => (
                <button
                  key={c.id}
                  onClick={() => {
                    onAssign(c.id);
                    onDismiss();
                  }}
                  className="flex w-full items-center justify-between rounded-md px-2 py-1.5 text-xs hover:bg-accent"
                >
                  <span>{c.name}</span>
                  <span className="text-[10px] text-muted-foreground">{c.meetingCount} mtgs</span>
                </button>
              ))}
          </div>
          <button
            onClick={() => setCreating(true)}
            className="mt-1 flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-xs hover:bg-accent"
          >
            <Plus className="size-3.5" /> Create new customer…
          </button>
          {current && (
            <button
              onClick={() => {
                onAssign(null);
                onDismiss();
              }}
              className="mt-1 flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-xs text-muted-foreground hover:bg-accent hover:text-destructive"
            >
              <Unlink className="size-3.5" /> Unassign customer
            </button>
          )}
        </>
      )}

      {creating && (
        <div className="space-y-1">
          <input
            ref={inputRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commitCreate}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitCreate();
              if (e.key === "Escape") setCreating(false);
            }}
            placeholder="Customer name"
            className="w-full rounded-md border bg-background px-2 py-1 text-xs outline-none focus:border-ring"
          />
          <p className="text-[10px] text-muted-foreground">
            Enter to create + assign, Esc to cancel.
          </p>
        </div>
      )}
    </div>
  );
}
