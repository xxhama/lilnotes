/**
 * Personas view. Lists all named personas with their voiceprint gallery
 * counts, supports rename and delete (which removes voiceprints and nulls
 * speaker links). Reached from the sidebar nav.
 */
import { useCallback, useEffect, useState } from "react";
import { Plus, Trash2, Users } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  createPersona,
  deleteAllVoiceprints,
  deletePersona,
  listPersonas,
  onVoiceprintsEnrolled,
  renamePersona,
  type Persona,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";

export default function PersonasView() {
  const [personas, setPersonas] = useState<Persona[]>([]);
  const [draft, setDraft] = useState("");
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(() => {
    listPersonas()
      .then(setPersonas)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(reload, [reload]);

  // Confirming a speaker in a meeting drops the old persona's voiceprint at
  // once but enrolls the new one in the background (seconds). Refresh counts
  // when that finishes, so opening this page mid-enrollment doesn't show the
  // removal without the addition.
  useTauriEvent(onVoiceprintsEnrolled, reload);

  const add = useCallback(async () => {
    const name = draft.trim();
    if (!name) return;
    try {
      await createPersona(name);
      setDraft("");
      reload();
    } catch (e) {
      setError(String(e));
    }
  }, [draft, reload]);

  const commitRename = useCallback(
    async (id: number) => {
      const name = editDraft.trim();
      setEditingId(null);
      if (!name) return;
      try {
        await renamePersona(id, name);
        reload();
      } catch (e) {
        setError(String(e));
      }
    },
    [editDraft, reload],
  );

  const remove = useCallback(
    async (id: number) => {
      if (!confirm("Delete this persona and all its stored voiceprints?")) return;
      try {
        await deletePersona(id);
        reload();
      } catch (e) {
        setError(String(e));
      }
    },
    [reload],
  );

  const removeAll = useCallback(async () => {
    if (
      !confirm(
        "Delete ALL stored voiceprints for every persona? Personas stay, but recognition will reset.",
      )
    )
      return;
    try {
      await deleteAllVoiceprints();
      reload();
    } catch (e) {
      setError(String(e));
    }
  }, [reload]);

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-8 pt-4">
      <div>
        <h1 className="flex items-center gap-2 text-lg font-semibold tracking-tight">
          <Users className="size-5" /> Personas
        </h1>
        <p className="text-sm text-muted-foreground">
          Named speakers LilNotes learns across meetings. Each persona holds a gallery of
          voiceprints that grows when you confirm an identity.
        </p>
      </div>

      <div className="flex gap-2">
        <Input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
          placeholder="New persona name…"
          className="h-9 flex-1 rounded-md bg-background px-2 text-sm"
        />
        <Button size="sm" onClick={add} disabled={!draft.trim()}>
          <Plus /> Add
        </Button>
      </div>

      {error && <p className="text-sm text-destructive">{error}</p>}

      <div className="divide-y rounded-xl border bg-card">
        {personas.length === 0 && (
          <div className="p-4 text-sm text-muted-foreground">
            No personas yet. Confirm a speaker in any meeting to create one.
          </div>
        )}
        {personas.map((p) => (
          <div key={p.id} className="flex items-center justify-between gap-3 p-3">
            <div className="min-w-0 flex-1">
              {editingId === p.id ? (
                <Input
                  autoFocus
                  value={editDraft}
                  onChange={(e) => setEditDraft(e.target.value)}
                  onBlur={() => commitRename(p.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") commitRename(p.id);
                    if (e.key === "Escape") setEditingId(null);
                  }}
                  className="w-full rounded-md bg-background px-2 py-0.5 text-sm"
                />
              ) : (
                <button
                  onClick={() => {
                    setEditingId(p.id);
                    setEditDraft(p.displayName);
                  }}
                  className="block w-full truncate text-left text-sm font-medium hover:opacity-80"
                >
                  {p.displayName}
                </button>
              )}
              <p className="text-xs text-muted-foreground">
                {p.voiceprintCount} voiceprint{p.voiceprintCount === 1 ? "" : "s"}
              </p>
            </div>
            <Button
              size="icon"
              variant="ghost"
              className="size-7 text-muted-foreground hover:text-destructive"
              onClick={() => remove(p.id)}
              aria-label={`Delete ${p.displayName}`}
            >
              <Trash2 className="size-3.5" />
            </Button>
          </div>
        ))}
      </div>

      {personas.length > 0 && (
        <Button variant="outline" size="sm" onClick={removeAll}>
          <Trash2 className="size-3.5" /> Delete all voiceprints
        </Button>
      )}

      <p className="text-xs text-muted-foreground">
        Voiceprints are stored locally in{" "}
        <code className="rounded bg-secondary px-1">
          ~/Library/Application Support/co.elastic.lilnote/lilnotes.sqlite3
        </code>
        . They never leave your Mac.
      </p>
    </div>
  );
}
