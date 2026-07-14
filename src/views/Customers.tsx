/**
 * Customers list view. Shows all customers with avatars, meeting counts,
 * and create/rename/delete actions. Clicking a customer navigates to the
 * customer detail page.
 */
import { useCallback, useEffect, useState } from "react";
import { Building2, Loader2, Plus, Trash2 } from "lucide-react";

import CustomerAvatar from "@/components/CustomerAvatar";
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
import { createCustomer, deleteCustomer, listCustomers, type CustomerSummary } from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
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

/** Customers: top-level list of accounts, each linking to its detail view. */
export default function CustomersView({ onNavigate }: Props) {
  const [customers, setCustomers] = useState<CustomerSummary[] | null>(null);
  const [draft, setDraft] = useState("");
  const [pendingDelete, setPendingDelete] = useState<CustomerSummary | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(() => {
    listCustomers()
      .then(setCustomers)
      .catch((e) => {
        setCustomers([]);
        setError(String(e));
      });
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  const add = useCallback(async () => {
    const name = draft.trim();
    if (!name) return;
    try {
      await createCustomer(name);
      setDraft("");
      reload();
    } catch (e) {
      setError(String(e));
    }
  }, [draft, reload]);

  const confirmDelete = useCallback(async () => {
    if (!pendingDelete) return;
    setDeleting(true);
    try {
      await deleteCustomer(pendingDelete.id);
      setPendingDelete(null);
      reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setDeleting(false);
    }
  }, [pendingDelete, reload]);

  if (customers === null) return <div className="p-8" />;

  if (customers.length === 0) {
    return (
      <div className="mx-auto max-w-2xl space-y-6 p-8 pt-4">
        <div>
          <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
            <Building2 className="size-5" /> Customers
          </h1>
          <p className="text-sm text-muted-foreground">
            Group meetings by customer to see a per-account birds-eye view.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && add()}
            placeholder="Add a customer…"
            className="h-9 flex-1 rounded-md bg-card px-3 text-sm"
          />
          <Button size="sm" onClick={add} disabled={!draft.trim()}>
            <Plus /> Add
          </Button>
        </div>
        <div className="rounded-xl border bg-card p-8 text-center text-sm text-muted-foreground">
          No customers yet. Add one above to start grouping meetings.
        </div>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-8 pt-4">
      <div>
        <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
          <Building2 className="size-5" /> Customers
        </h1>
        <p className="text-sm text-muted-foreground">
          Group meetings by customer to see a per-account birds-eye view.
        </p>
      </div>

      <div className="flex items-center gap-2">
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
          placeholder="Add a customer…"
          className="h-9 flex-1 rounded-md border bg-card px-3 text-sm outline-none focus:border-ring"
        />
        <Button size="sm" onClick={add} disabled={!draft.trim()}>
          <Plus /> Add
        </Button>
      </div>

      {error && <p className="text-sm text-destructive">{error}</p>}

      <div className="divide-y rounded-xl border bg-card">
        {customers.map((c) => (
          <button
            key={c.id}
            onClick={() => onNavigate({ name: "customer", customerId: String(c.id) })}
            className="group flex w-full items-center gap-3 p-3 text-left transition-colors hover:bg-accent/50"
          >
            <CustomerAvatar name={c.name} className="size-9 text-sm" />
            <div className="min-w-0 flex-1 space-y-0.5">
              <div className="truncate text-sm font-medium">{c.name}</div>
              <div className="flex items-center gap-3 text-xs text-muted-foreground">
                <span>
                  {c.meetingCount} {c.meetingCount === 1 ? "meeting" : "meetings"}
                </span>
                <span>Last {fmtDate(c.lastMeetingAtMs)}</span>
              </div>
            </div>
            <Button
              size="icon"
              variant="ghost"
              className="size-8 opacity-0 transition-opacity group-hover:opacity-100"
              onClick={(e) => {
                e.stopPropagation();
                setPendingDelete(c);
              }}
              aria-label={`Delete ${c.name}`}
              asChild
            >
              <span>
                <Trash2 className="size-4 text-muted-foreground" />
              </span>
            </Button>
          </button>
        ))}
      </div>

      <AlertDialog
        open={pendingDelete != null}
        onOpenChange={(open) => {
          if (!open && !deleting) setPendingDelete(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete customer?</AlertDialogTitle>
            <AlertDialogDescription>
              This deletes{" "}
              <span className="font-medium text-foreground">{pendingDelete?.name}</span>. Its
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
