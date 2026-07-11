import { useCallback, useEffect, useState } from "react";
import { CheckCircle2, Download, RefreshCw, X, Zap } from "lucide-react";

import { Button } from "@/components/ui/button";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  cancelNativeModelDownload,
  downloadNativeModel,
  listNativeModels,
  nativeModelStatus,
  onModelProgress,
  type AppSettings,
  type DownloadProgress,
  type NativeLlmModelInfo,
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
 * Built-in LLM model management. Renders as a fragment of rows (no card
 * wrapper) — `Settings.tsx` provides the card shell so the backend selector
 * and model list share one card. Emits a status header row + a RadioGroup
 * of model rows (download + select inline, no separate dropdown).
 */
export default function NativeModelManager({ settings, onSave }: Props) {
  const [models, setModels] = useState<NativeLlmModelInfo[]>([]);
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});
  const [loaded, setLoaded] = useState<boolean>(false);

  const refresh = useCallback(async () => {
    setModels(await listNativeModels().catch(() => []));
    setLoaded(await nativeModelStatus().catch(() => false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useTauriEvent(onModelProgress, (p) => {
    // Only track progress for native LLM model ids.
    if (!models.some((m) => m.id === p.id)) return;
    setProgress((prev) => ({ ...prev, [p.id]: p }));
    if (p.done) {
      refresh();
      if (!p.error || p.error === "cancelled") {
        // Auto-select the newly downloaded model if none is set.
        if (!settings.summaryModel) {
          onSave({ ...settings, summaryModel: p.id });
        }
      }
      setTimeout(
        () =>
          setProgress((prev) => {
            const next = { ...prev };
            delete next[p.id];
            return next;
          }),
        1500,
      );
    }
  });

  const startDownload = useCallback(
    (id: string) => {
      setProgress((prev) => ({
        ...prev,
        [id]: { id, downloaded: 0, total: null, done: false, error: null },
      }));
      downloadNativeModel(id)
        .catch(() => {})
        .finally(refresh);
    },
    [refresh],
  );

  return (
    <>
      {/* Status header row */}
      <div className="flex items-center justify-between gap-4 p-4">
        <div className="space-y-0.5">
          <div className="flex items-center gap-2 text-sm font-medium">
            Built-in engine
            <span
              className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium ${
                loaded ? "bg-success/10 text-success" : "bg-secondary text-muted-foreground"
              }`}
            >
              {loaded ? (
                <>
                  <CheckCircle2 className="size-3" /> Loaded in memory
                </>
              ) : (
                "Not loaded"
              )}
            </span>
          </div>
          <p className="text-xs text-muted-foreground">
            Runs on-device with Metal acceleration. Loads on demand and frees memory after 5 min
            idle.
          </p>
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

      {/* Model radio rows */}
      <RadioGroup
        value={settings.summaryModel ?? undefined}
        onValueChange={(v) => onSave({ ...settings, summaryModel: v })}
        className="grid gap-0 divide-y"
      >
        {models.map((m) => {
          const p = progress[m.id];
          const downloading = p && !p.done;
          const pct = downloading && p.total ? Math.round((p.downloaded / p.total) * 100) : null;
          return (
            <div key={m.id} className="flex items-center gap-4 p-4">
              <RadioGroupItem value={m.id} disabled={!m.downloaded} />
              <div className="min-w-0 flex-1 space-y-0.5">
                <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
                  <span data-selectable>{m.label}</span>
                  <span className="text-xs font-normal text-muted-foreground">
                    ~{fmtBytes(m.approxBytes)}
                  </span>
                  {m.id === "qwen3.5-4b" && (
                    <span className="inline-flex items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-medium text-primary">
                      <Zap className="size-3" /> recommended
                    </span>
                  )}
                  {m.downloaded && (
                    <span className="inline-flex items-center gap-1 rounded-full bg-success/10 px-2 py-0.5 text-[11px] font-medium text-success">
                      <CheckCircle2 className="size-3" /> Downloaded
                    </span>
                  )}
                </div>
                <p className="text-xs text-muted-foreground">{m.note}</p>
                {downloading && (
                  <div className="flex items-center gap-2 pt-1">
                    <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-secondary">
                      <div
                        className="h-full rounded-full bg-primary transition-[width]"
                        style={{ width: `${pct ?? 5}%` }}
                      />
                    </div>
                    <span className="w-24 text-right text-[11px] tabular-nums text-muted-foreground">
                      {pct !== null ? `${pct}% · ${fmtBytes(p.downloaded)}` : "starting…"}
                    </span>
                    <Button
                      size="icon"
                      variant="ghost"
                      className="size-6"
                      onClick={() => cancelNativeModelDownload(m.id)}
                      aria-label="Cancel download"
                    >
                      <X className="size-3.5" />
                    </Button>
                  </div>
                )}
                {p?.error && p.error !== "cancelled" && (
                  <p className="pt-1 text-xs text-destructive">{p.error}</p>
                )}
              </div>
              {!m.downloaded && !downloading && (
                <Button size="sm" variant="outline" onClick={() => startDownload(m.id)}>
                  <Download /> Download
                </Button>
              )}
            </div>
          );
        })}
      </RadioGroup>
    </>
  );
}
