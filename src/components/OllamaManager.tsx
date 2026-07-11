import { useCallback, useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { CheckCircle2, Download, ExternalLink, RefreshCw, X, Zap } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  cancelOllamaPull,
  listOllamaModels,
  ollamaStatus,
  onOllamaPull,
  pullOllamaModel,
  suggestedOllamaModels,
  type AppSettings,
  type OllamaModels,
  type PullProgress,
  type SuggestedModel,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";

function fmtBytes(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)} GB`;
  if (n >= 1_000_000) return `${Math.round(n / 1_000_000)} MB`;
  return `${Math.round(n / 1000)} kB`;
}

interface Props {
  settings: AppSettings;
  onSave: (s: AppSettings) => void;
}

/**
 * Ollama status + summary model picker + curated "models to pull" list
 * with live progress. All model management happens through the Ollama HTTP
 * API — the terminal command is shown only as a fallback when a pull fails.
 */
export default function OllamaManager({ settings, onSave }: Props) {
  const [reachable, setReachable] = useState<boolean | null>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [models, setModels] = useState<OllamaModels | null>(null);
  const [suggested, setSuggested] = useState<SuggestedModel[]>([]);
  const [pulls, setPulls] = useState<Record<string, PullProgress>>({});
  const [failed, setFailed] = useState<Record<string, string>>({});

  const refresh = useCallback(async () => {
    const s = await ollamaStatus();
    setReachable(s.reachable);
    setVersion(s.version);
    if (s.reachable) {
      setModels(await listOllamaModels().catch(() => null));
      setSuggested(await suggestedOllamaModels().catch(() => []));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useTauriEvent(onOllamaPull, (p) => {
    setPulls((prev) => ({ ...prev, [p.model]: p }));
    if (p.done) {
      if (p.error && p.error !== "cancelled") {
        setFailed((prev) => ({ ...prev, [p.model]: p.error! }));
      }
      if (p.status === "success") {
        // Refresh the installed list and auto-select the new model.
        refresh().then(() => onSave({ ...settings, summaryModel: p.model }));
      }
      setTimeout(
        () =>
          setPulls((prev) => {
            const next = { ...prev };
            delete next[p.model];
            return next;
          }),
        1500,
      );
    }
  });

  const startPull = useCallback((tag: string) => {
    setFailed((prev) => {
      const next = { ...prev };
      delete next[tag];
      return next;
    });
    setPulls((prev) => ({
      ...prev,
      [tag]: { model: tag, status: "starting", completed: 0, total: 0, done: false, error: null },
    }));
    pullOllamaModel(tag).catch(() => {});
  }, []);

  if (reachable === null) return null;

  if (!reachable) {
    return (
      <div className="space-y-3 rounded-xl border bg-card p-4 text-sm">
        <p className="font-medium">Ollama isn't running</p>
        <p className="text-xs leading-relaxed text-muted-foreground">
          Summaries use a local model served by Ollama at localhost:11434. Install it from
          ollama.com and launch it — the rest of LilNotes works fine without it.
        </p>
        <div className="flex gap-2">
          <Button
            size="sm"
            variant="outline"
            onClick={() => openUrl("https://ollama.com/download")}
          >
            <ExternalLink /> Get Ollama
          </Button>
          <Button size="sm" variant="ghost" onClick={refresh}>
            <RefreshCw /> Check again
          </Button>
        </div>
      </div>
    );
  }

  const activeModel = settings.summaryModel ?? models?.active ?? null;

  return (
    <div className="space-y-3">
      {/* Status + active model picker */}
      <div className="divide-y rounded-xl border bg-card">
        <div className="flex items-center justify-between gap-4 p-4">
          <div className="space-y-0.5">
            <div className="flex items-center gap-2 text-sm font-medium">
              Ollama
              <span className="inline-flex items-center gap-1 rounded-full bg-green-600/10 px-2 py-0.5 text-xs font-medium text-green-700 dark:text-green-400">
                <CheckCircle2 className="size-3" /> Running{version ? ` · v${version}` : ""}
              </span>
            </div>
            <p className="text-xs text-muted-foreground">localhost:11434</p>
          </div>
          <Button
            size="icon"
            variant="ghost"
            className="size-8"
            onClick={refresh}
            aria-label="Refresh"
          >
            <RefreshCw className="size-4" />
          </Button>
        </div>

        <div className="flex items-center justify-between gap-4 p-4">
          <div className="space-y-0.5">
            <div className="text-sm font-medium">Summary model</div>
            <p className="text-xs text-muted-foreground">
              {models && models.installed.length === 0
                ? "No models installed yet — pull one below."
                : "Used for meeting summaries; also selectable per meeting."}
            </p>
          </div>
          {models && models.installed.length > 0 && (
            <select
              value={activeModel ?? ""}
              onChange={(e) => onSave({ ...settings, summaryModel: e.target.value })}
              className="h-8 max-w-56 rounded-md border bg-card px-2 text-xs outline-none focus:border-ring"
            >
              {models.installed.map((m) => (
                <option key={m.name} value={m.name}>
                  {m.name} ({fmtBytes(m.sizeBytes)})
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      {/* Curated suggestions */}
      <div className="divide-y rounded-xl border bg-card">
        {suggested.map((s) => {
          const pull = pulls[s.tag];
          const pulling = pull && !pull.done;
          const pct =
            pulling && pull.total > 0 ? Math.round((pull.completed / pull.total) * 100) : null;
          const installed = s.installed || s.mlxInstalled;
          return (
            <div key={s.tag} className="flex items-center gap-4 p-4">
              <div className="min-w-0 flex-1 space-y-0.5">
                <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
                  <span data-selectable>{s.tag}</span>
                  <span className="text-xs font-normal text-muted-foreground">
                    ~{s.approxDownload}
                  </span>
                  {s.tier === "default" && (
                    <span className="inline-flex items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-medium text-primary">
                      <Zap className="size-3" /> recommended
                    </span>
                  )}
                  {installed && (
                    <span className="inline-flex items-center gap-1 rounded-full bg-green-600/10 px-2 py-0.5 text-[11px] font-medium text-green-700 dark:text-green-400">
                      <CheckCircle2 className="size-3" />
                      {s.mlxInstalled ? "installed (mlx)" : "installed"}
                    </span>
                  )}
                </div>
                <p className="text-xs text-muted-foreground">{s.note}</p>
                {pulling && (
                  <div className="flex items-center gap-2 pt-1">
                    <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-secondary">
                      <div
                        className="h-full rounded-full bg-primary transition-[width]"
                        style={{ width: `${pct ?? 3}%` }}
                      />
                    </div>
                    <span className="w-24 truncate text-right text-[11px] tabular-nums text-muted-foreground">
                      {pct !== null ? `${pct}% · ${fmtBytes(pull.completed)}` : pull.status}
                    </span>
                    <Button
                      size="icon"
                      variant="ghost"
                      className="size-6"
                      onClick={() => cancelOllamaPull(s.tag)}
                      aria-label="Cancel pull"
                    >
                      <X className="size-3.5" />
                    </Button>
                  </div>
                )}
                {failed[s.tag] && (
                  <div className="space-y-1 pt-1 text-xs">
                    <p className="text-destructive">{failed[s.tag]}</p>
                    <p className="text-muted-foreground">
                      Fallback: run{" "}
                      <code className="rounded bg-secondary px-1 py-0.5" data-selectable>
                        ollama pull {s.tag}
                      </code>{" "}
                      in a terminal, or see{" "}
                      <button
                        className="cursor-pointer underline"
                        onClick={() => openUrl(`https://ollama.com/library/${s.tag.split(":")[0]}`)}
                      >
                        the model page
                      </button>
                      , then Refresh above.
                    </p>
                  </div>
                )}
              </div>
              {!installed && !pulling && (
                <Button size="sm" variant="outline" onClick={() => startPull(s.tag)}>
                  <Download /> Pull
                </Button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
