import { useEffect, useRef, useState } from "react";
import { Check, Plus, Unlink, X } from "lucide-react";

import type { Persona, PersonaScore } from "@/lib/ipc";

interface Props {
  rawLabel: string;
  current: PersonaScore | null;
  /** All personas for the "choose different" list. */
  personas: Persona[];
  onConfirm: (personaId: number) => void;
  onCreatePersona: (name: string) => Promise<number>;
  /** Close the popover without side effects. */
  onDismiss: () => void;
  /** Remove the current persona link (deliberate action). */
  onUnlink?: () => void;
}

/**
 * Picker for confirming / choosing / creating / dismissing a persona for a
 * diarized speaker. Free-text create doubles as the legacy rename path.
 */
export default function SpeakerPersonaPicker({
  rawLabel,
  current,
  personas,
  onConfirm,
  onCreatePersona,
  onDismiss,
  onUnlink,
}: Props) {
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (creating) inputRef.current?.focus();
  }, [creating]);

  const pct = (s: number) => `${Math.round(Math.max(0, Math.min(1, s)) * 100)}%`;

  const commitCreate = async () => {
    const name = draft.trim();
    if (!name) return;
    const id = await onCreatePersona(name);
    onConfirm(id);
  };

  return (
    <div
      className="absolute z-50 w-64 rounded-lg border bg-popover p-2 shadow-md"
      // crude click-outside dismissal
      onClick={(e) => e.stopPropagation()}
    >
      <div className="mb-1 flex items-center justify-between">
        <span className="text-[11px] font-medium text-muted-foreground">Assign {rawLabel}</span>
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
          onClick={() => onConfirm(current.personaId)}
          className="mb-1 flex w-full items-center justify-between rounded-md bg-primary/10 px-2 py-1.5 text-xs hover:bg-primary/15"
        >
          <span className="flex items-center gap-1.5 font-medium">
            <Check className="size-3.5" /> {current.displayName}
          </span>
          <span className="text-[10px] text-muted-foreground">{pct(current.score)}</span>
        </button>
      )}

      {!creating && (
        <>
          <div className="max-h-40 overflow-y-auto">
            {personas
              .filter((p) => p.id !== current?.personaId)
              .map((p) => (
                <button
                  key={p.id}
                  onClick={() => onConfirm(p.id)}
                  className="flex w-full items-center justify-between rounded-md px-2 py-1.5 text-xs hover:bg-accent"
                >
                  <span>{p.displayName}</span>
                  <span className="text-[10px] text-muted-foreground">
                    {p.voiceprintCount} prints
                  </span>
                </button>
              ))}
          </div>
          <button
            onClick={() => setCreating(true)}
            className="mt-1 flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-xs hover:bg-accent"
          >
            <Plus className="size-3.5" /> Create new persona…
          </button>
          {current && onUnlink && (
            <button
              onClick={onUnlink}
              className="mt-1 flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-xs text-muted-foreground hover:bg-accent hover:text-destructive"
            >
              <Unlink className="size-3.5" /> Unlink persona
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
            placeholder="Persona name"
            className="w-full rounded-md border bg-background px-2 py-1 text-xs outline-none focus:border-ring"
          />
          <p className="text-[10px] text-muted-foreground">
            Enter to create + confirm, Esc to cancel.
          </p>
        </div>
      )}
    </div>
  );
}
