import { useEffect, useRef, useState, type ReactNode } from "react";
import { Check, Loader2, Plus, Unlink } from "lucide-react";

import {
  Command,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

export interface PickerItem {
  id: number;
  label: string;
  sublabel?: string;
}

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The trigger element, composed via PopoverTrigger asChild (Radix wires the
   * toggle; do not put an onClick that flips `open` on it). */
  trigger: ReactNode;
  /** Existing entities to list (exclude the current one). */
  items: PickerItem[];
  currentId: number | null;
  currentLabel?: string;
  currentSublabel?: string;
  onPick: (id: number) => void;
  /** Create a new entity and return its id; the combobox auto-picks it.
   * Optional so a future call site that only needs assign/unassign can omit it
   * (the "Create new <noun>…" entry is gated on its presence). */
  onCreate?: (name: string) => Promise<number>;
  onUnassign?: () => void;
  unassignLabel: string;
  /** Lowercase noun used in placeholder + empty/help copy ("customer"/"persona"). */
  createNoun: string;
  placeholder?: string;
  align?: "start" | "center" | "end";
  className?: string;
}

/**
 * Searchable assign / create / unassign combobox built on shadcn Popover +
 * cmdk Command. Replaces the two hand-built picker popovers (customer assign
 * in MeetingDetail, speaker persona in TranscriptPane) so the dropdowns
 * render consistently across macOS versions.
 *
 * Two-step create: an always-visible "Create new <noun>…" entry in the Actions
 * group opens an inline name form (shadcn Input + Button). Enter creates +
 * assigns, Esc cancels back to the list. The search box filters the existing
 * list and its query is carried into the create form, so the type-first habit
 * still works (type a name, click Create, Enter). Filtering is done manually
 * (shouldFilter={false}) so the Current + Actions rows stay selectable
 * regardless of the query. Uses only semantic color tokens (popover/primary/
 * accent/muted) so it themes correctly in light + dark — no hardcoded colors.
 */
export default function PickerCombobox({
  open,
  onOpenChange,
  trigger,
  items,
  currentId,
  currentLabel,
  currentSublabel,
  onPick,
  onCreate,
  onUnassign,
  unassignLabel,
  createNoun,
  placeholder,
  align = "start",
  className,
}: Props) {
  const [search, setSearch] = useState("");
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);

  // Reset everything whenever the popover closes so the next open starts fresh.
  useEffect(() => {
    if (!open) {
      setSearch("");
      setCreating(false);
      setDraft("");
      setBusy(false);
    }
  }, [open]);

  // Auto-focus the name field when entering create mode.
  useEffect(() => {
    if (creating) nameRef.current?.focus();
  }, [creating]);

  const query = search.trim().toLowerCase();
  const filtered = query ? items.filter((it) => it.label.toLowerCase().includes(query)) : items;

  const pick = (id: number) => {
    onPick(id);
    onOpenChange(false);
  };

  const unassign = () => {
    onUnassign?.();
    onOpenChange(false);
  };

  // Enter create mode, carrying any query the user already typed into the name
  // field so it isn't lost.
  const enterCreate = () => {
    setDraft(search.trim());
    setCreating(true);
  };

  const commitCreate = async () => {
    if (!onCreate) return;
    const name = draft.trim();
    if (!name) return;
    setBusy(true);
    try {
      const id = await onCreate(name); // caller creates the entity, returns its id
      onPick(id); // caller assigns (set_meeting_customer / confirm_speaker_persona)
      onOpenChange(false);
    } catch (e) {
      // Leave create mode open so a transient failure isn't silently swallowed.
      console.error("create failed", e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Popover open={open} onOpenChange={onOpenChange}>
      <PopoverTrigger asChild>{trigger}</PopoverTrigger>
      <PopoverContent align={align} className={cn("w-64 p-0", className)}>
        {creating ? (
          <div className="space-y-2 p-2">
            <label className="text-xs text-muted-foreground">Create new {createNoun}</label>
            <Input
              ref={nameRef}
              value={draft}
              disabled={busy}
              placeholder={`${cap(createNoun)} name…`}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void commitCreate();
                } else if (e.key === "Escape") {
                  setCreating(false);
                }
              }}
            />
            <div className="flex items-center justify-between gap-2">
              <span className="text-[10px] text-muted-foreground">
                {busy ? "Creating…" : "Enter to create + assign · Esc to cancel"}
              </span>
              <Button
                size="sm"
                disabled={busy || draft.trim().length === 0}
                onClick={() => void commitCreate()}
              >
                {busy ? (
                  <Loader2 className="size-3.5 animate-spin" />
                ) : (
                  <Plus className="size-3.5" />
                )}
                Create
              </Button>
            </div>
          </div>
        ) : (
          <Command shouldFilter={false}>
            <CommandInput
              value={search}
              onValueChange={setSearch}
              placeholder={placeholder ?? `Search ${createNoun}s…`}
            />
            <CommandList>
              {currentId != null && currentLabel && (
                <CommandGroup heading="Current">
                  <CommandItem value={`current-${currentId}`} onSelect={() => pick(currentId)}>
                    <Check className="size-4" /> {currentLabel}
                    {currentSublabel && (
                      <span className="ml-auto text-[10px] text-muted-foreground">
                        {currentSublabel}
                      </span>
                    )}
                  </CommandItem>
                </CommandGroup>
              )}
              <CommandGroup heading={currentId != null ? "Switch to" : "Assign"}>
                {filtered.length === 0 ? (
                  <p className="px-2 py-3 text-center text-xs text-muted-foreground">
                    {items.length === 0 ? `No ${createNoun}s yet.` : "No matches."}
                  </p>
                ) : (
                  filtered.map((it) => (
                    <CommandItem key={it.id} value={`${it.id}`} onSelect={() => pick(it.id)}>
                      {it.label}
                      {it.sublabel && (
                        <span className="ml-auto text-[10px] text-muted-foreground">
                          {it.sublabel}
                        </span>
                      )}
                    </CommandItem>
                  ))
                )}
              </CommandGroup>
              {(onCreate || onUnassign) && (
                <CommandGroup heading="Actions">
                  {onCreate && (
                    <CommandItem value="__create__" onSelect={enterCreate}>
                      <Plus className="size-4" /> Create new {createNoun}…
                    </CommandItem>
                  )}
                  {onUnassign && (
                    <CommandItem value="__unassign__" onSelect={unassign}>
                      <Unlink className="size-4" /> {unassignLabel}
                    </CommandItem>
                  )}
                </CommandGroup>
              )}
            </CommandList>
          </Command>
        )}
      </PopoverContent>
    </Popover>
  );
}

/** Capitalize the first letter of `s` (for placeholder copy like "Customer name…"). */
function cap(s: string): string {
  return s.length === 0 ? s : s[0].toUpperCase() + s.slice(1);
}
