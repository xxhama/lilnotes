import { useCallback, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, Loader2, RefreshCw, Sparkles } from "lucide-react";

import { Button } from "@/components/ui/button";
import ThinkingDisplay from "@/components/ThinkingDisplay";
import { Markdown } from "@/components/SummaryPanel";
import {
  getSettings,
  listCustomerSummaries,
  listNativeModels,
  listOllamaModels,
  ollamaStatus,
  onCustomerSummaryToken,
  summarizeCustomer,
  type AppSettings,
  type CustomerRollupRow,
  type NativeLlmModelInfo,
  type OllamaModels,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";

interface Props {
  customerId: number;
  /** Number of this customer's meetings that have a saved summary. */
  meetingCountWithSummary: number;
}

/**
 * "Customer at a glance" rollup: condenses the AI summaries of a customer's
 * recent meetings into a short topics overview, streamed token-by-token and
 * persisted. Reuses the same LLM backend (native llama.cpp / Ollama) and
 * thinking display as the per-meeting SummaryPanel.
 */
export default function CustomerSummaryPanel({ customerId, meetingCountWithSummary }: Props) {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [nativeModels, setNativeModels] = useState<NativeLlmModelInfo[]>([]);
  const [ollamaReachable, setOllamaReachable] = useState<boolean | null>(null);
  const [ollamaModels, setOllamaModels] = useState<OllamaModels | null>(null);
  const [model, setModel] = useState<string | null>(null);
  const [saved, setSaved] = useState<CustomerRollupRow | null>(null);
  const [streaming, setStreaming] = useState<string | null>(null);
  const [thinkingText, setThinkingText] = useState<string | null>(null);
  const [thinkingDuration, setThinkingDuration] = useState<number | null>(null);
  const thinkingStartRef = useRef<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [generationKey, setGenerationKey] = useState(0);

  const refresh = useCallback(async () => {
    const s = await getSettings().catch(() => null);
    setSettings(s);
    if (s) {
      if (s.summaryBackend === "ollama") {
        const status = await ollamaStatus();
        setOllamaReachable(status.reachable);
        if (status.reachable) {
          const m = await listOllamaModels().catch(() => null);
          setOllamaModels(m);
          setModel((prev) => prev ?? m?.active ?? null);
        }
      } else {
        const m = await listNativeModels().catch(() => []);
        setNativeModels(m);
        const downloaded = m.filter((x) => x.downloaded);
        setModel((prev) => prev ?? downloaded[0]?.id ?? null);
      }
    }
    const rows = await listCustomerSummaries(customerId).catch(() => []);
    setSaved(rows[0] ?? null);
  }, [customerId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useTauriEvent(onCustomerSummaryToken, (e) => {
    if (e.customerId !== customerId) return;
    if (e.isThinking) {
      if (thinkingStartRef.current === null) {
        thinkingStartRef.current = Date.now();
      }
      setThinkingText((prev) => (prev ?? "") + e.token);
    } else {
      if (thinkingStartRef.current !== null) {
        setThinkingDuration(Math.round((Date.now() - thinkingStartRef.current) / 1000));
        thinkingStartRef.current = null;
      }
      setThinkingText(null);
      setStreaming((prev) => (prev ?? "") + e.token);
    }
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  });

  const generate = useCallback(async () => {
    setBusy(true);
    setError(null);
    setStreaming("");
    setThinkingText(null);
    setThinkingDuration(null);
    thinkingStartRef.current = null;
    setGenerationKey((k) => k + 1);
    try {
      const result = await summarizeCustomer(customerId, model ?? undefined);
      setSaved({
        id: result.summaryId,
        model: result.model,
        content: result.content,
        createdAtMs: Date.now(),
        meetingCount: result.meetingCount,
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setStreaming(null);
      setThinkingText(null);
      setBusy(false);
    }
  }, [customerId, model]);

  const isOllama = settings?.summaryBackend === "ollama";

  // --- Ollama backend not running: setup panel -------------------------
  if (isOllama && ollamaReachable === false) {
    return (
      <div className="space-y-3 p-4">
        <h2 className="text-sm font-semibold">Recent topics</h2>
        <div className="space-y-3 rounded-lg border bg-card p-4 text-sm">
          <p className="font-medium">Ollama isn't running</p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            Customer rollups are generated on-device by a local model served by Ollama. Launch it
            and come back.
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
      </div>
    );
  }

  // --- Native backend, no model downloaded: prompt to download ---------
  const nativeDownloaded = nativeModels.filter((m) => m.downloaded);
  if (!isOllama && settings && nativeDownloaded.length === 0) {
    return (
      <div className="space-y-3 p-4">
        <h2 className="text-sm font-semibold">Recent topics</h2>
        <div className="space-y-3 rounded-lg border bg-card p-4 text-sm">
          <p className="font-medium">No summary model downloaded</p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            Download a built-in model in Settings → Summaries to enable customer rollups.
          </p>
          <Button size="sm" variant="ghost" onClick={refresh}>
            <RefreshCw /> Check again
          </Button>
        </div>
      </div>
    );
  }

  const pickerModels = isOllama
    ? (ollamaModels?.installed.map((m) => ({ id: m.name, label: m.name })) ?? [])
    : nativeDownloaded.map((m) => ({ id: m.id, label: m.label }));
  const hasModels = pickerModels.length > 0;
  const showText = streaming !== null ? streaming : saved?.content;
  const canGenerate = meetingCountWithSummary >= 2;

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b px-4 h-11">
        <span className="text-xs font-medium text-muted-foreground">Recent topics</span>
        <div className="flex items-center gap-2">
          {hasModels && (
            <select
              value={model ?? ""}
              onChange={(e) => setModel(e.target.value)}
              disabled={busy}
              className="h-7 max-w-40 rounded-md border bg-card px-1.5 text-xs outline-none focus:border-ring"
            >
              {pickerModels.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
            </select>
          )}
          <Button size="sm" onClick={generate} disabled={busy || !hasModels || !canGenerate}>
            {busy ? <Loader2 className="animate-spin" /> : <Sparkles />}
            {saved || streaming !== null ? "Regenerate" : "Summarize"}
          </Button>
        </div>
      </div>

      <ThinkingDisplay
        key={generationKey}
        thinkingText={thinkingText}
        thinkingDuration={thinkingDuration}
        busy={busy}
      />

      {busy && thinkingText === null && streaming === "" && (
        <div className="flex items-center gap-1.5 border-b px-4 py-3 text-xs text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" />
          <span>Loading model…</span>
        </div>
      )}

      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto p-4">
        {error && <p className="pb-2 text-xs text-destructive">{error}</p>}

        {!canGenerate && !busy && (
          <p className="text-xs text-muted-foreground">
            Generate summaries on at least two of this customer's meetings first, then a rollup can
            be synthesized here.
          </p>
        )}

        {showText ? (
          <>
            <Markdown text={showText} />
            {streaming !== null && (
              <Loader2 className="mt-2 size-3.5 animate-spin text-muted-foreground" />
            )}
            {saved && streaming === null && (
              <p className="pt-3 text-[11px] text-muted-foreground/70">
                {saved.model} · rolled up {saved.meetingCount} meetings ·{" "}
                {new Date(saved.createdAtMs).toLocaleString()}
              </p>
            )}
          </>
        ) : (
          !error &&
          canGenerate &&
          !busy && (
            <p className="text-xs text-muted-foreground">
              Generate a bird's-eye rollup of this customer's recent meeting summaries.
            </p>
          )
        )}
      </div>
    </div>
  );
}
