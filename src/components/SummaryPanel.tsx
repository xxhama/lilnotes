import { useCallback, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, Loader2, RefreshCw, Sparkles } from "lucide-react";

import ThinkingDisplay from "@/components/ThinkingDisplay";

import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  getSettings,
  listNativeModels,
  listOllamaModels,
  listSummaries,
  ollamaStatus,
  onSummaryToken,
  summarizeMeeting,
  type AppSettings,
  type NativeLlmModelInfo,
  type OllamaModels,
  type SummaryRow,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";

interface Props {
  meetingId: number;
  hasTranscript: boolean;
  /** Called with a short AI-generated title when summarization produces one
   * (only when the meeting still had the default placeholder title). */
  onTitleGenerated?: (title: string) => void;
}

/** Minimal markdown rendering: headings + bullets; everything else as text. */
export function Markdown({ text, className = "text-sm" }: { text: string; className?: string }) {
  return (
    <div className={`space-y-1 leading-relaxed ${className}`} data-selectable>
      {text.split("\n").map((line, i) => {
        if (line.startsWith("## ")) {
          return (
            <h3
              key={i}
              className="pt-2 text-xs font-semibold tracking-wide text-muted-foreground uppercase"
            >
              {line.slice(3)}
            </h3>
          );
        }
        if (line.startsWith("# ")) {
          return (
            <h3 key={i} className="pt-2 text-sm font-semibold">
              {line.slice(2)}
            </h3>
          );
        }
        if (/^\s*[-*] /.test(line)) {
          return (
            <p key={i} className="flex gap-2 pl-1">
              <span className="text-muted-foreground">•</span>
              <span>{line.replace(/^\s*[-*] /, "").replace(/\*\*/g, "")}</span>
            </p>
          );
        }
        if (line.trim() === "") return <div key={i} className="h-1" />;
        return <p key={i}>{line.replace(/\*\*/g, "")}</p>;
      })}
    </div>
  );
}

/**
 * Streaming summary panel for the meeting detail view: shows the latest
 * saved summary, generates new ones token-by-token. Dispatches to the
 * built-in (llama.cpp) or Ollama backend based on settings.
 */
export default function SummaryPanel({ meetingId, hasTranscript, onTitleGenerated }: Props) {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [nativeModels, setNativeModels] = useState<NativeLlmModelInfo[]>([]);
  const [ollamaReachable, setOllamaReachable] = useState<boolean | null>(null);
  const [ollamaModels, setOllamaModels] = useState<OllamaModels | null>(null);
  const [model, setModel] = useState<string | null>(null);
  const [saved, setSaved] = useState<SummaryRow | null>(null);
  const [streaming, setStreaming] = useState<string | null>(null);
  const [thinkingText, setThinkingText] = useState<string | null>(null);
  const [thinkingDuration, setThinkingDuration] = useState<number | null>(null);
  const thinkingStartRef = useRef<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  // Bumped per generate() to remount ThinkingDisplay fresh.
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
    const rows = await listSummaries(meetingId).catch(() => []);
    setSaved(rows[0] ?? null);
  }, [meetingId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useTauriEvent(onSummaryToken, (e) => {
    if (e.meetingId !== meetingId) return;
    if (e.isThinking) {
      if (thinkingStartRef.current === null) {
        thinkingStartRef.current = Date.now();
      }
      setThinkingText((prev) => (prev ?? "") + e.token);
    } else {
      // First summary token → remove the thinking card immediately
      // (prevents jitter when the summary pushes it out)
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
      const result = await summarizeMeeting(meetingId, model ?? undefined);
      setSaved({
        id: result.summaryId,
        model: result.model,
        content: result.content,
        createdAtMs: Date.now(),
      });
      if (result.title) onTitleGenerated?.(result.title);
    } catch (e) {
      setError(String(e));
    } finally {
      setStreaming(null);
      setThinkingText(null);
      setBusy(false);
    }
  }, [meetingId, model, onTitleGenerated]);

  const isOllama = settings?.summaryBackend === "ollama";

  // --- Ollama backend not running: setup panel -------------------------
  if (isOllama && ollamaReachable === false) {
    return (
      <div className="space-y-3 p-4">
        <h2 className="text-sm font-semibold">Summary</h2>
        <div className="space-y-3 rounded-lg border bg-card p-4 text-sm">
          <p className="font-medium">Ollama isn't running</p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            Summaries are generated fully on-device by a local model served by Ollama. Install it
            from ollama.com, launch it once, and come back — no account needed, nothing leaves this
            Mac.
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
        <h2 className="text-sm font-semibold">Summary</h2>
        <div className="space-y-3 rounded-lg border bg-card p-4 text-sm">
          <p className="font-medium">No summary model downloaded</p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            Download a built-in model in Settings → Summaries to enable on-device summaries with
            Metal acceleration.
          </p>
          <Button size="sm" variant="ghost" onClick={refresh}>
            <RefreshCw /> Check again
          </Button>
        </div>
      </div>
    );
  }

  const showText = streaming !== null ? streaming : saved?.content;

  // Determine available models for the picker.
  const pickerModels = isOllama
    ? (ollamaModels?.installed.map((m) => ({ id: m.name, label: m.name })) ?? [])
    : nativeDownloaded.map((m) => ({ id: m.id, label: m.label }));
  const hasModels = pickerModels.length > 0;

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b px-4 h-11">
        <span className="text-xs font-medium text-muted-foreground">Summary</span>
        <div className="flex items-center gap-2">
          {hasModels && (
            <Select value={model ?? undefined} onValueChange={(v) => setModel(v)} disabled={busy}>
              <SelectTrigger size="sm" className="max-w-40 bg-card text-xs">
                <SelectValue placeholder="Model" />
              </SelectTrigger>
              <SelectContent position="popper">
                {pickerModels.map((m) => (
                  <SelectItem key={m.id} value={m.id}>
                    {m.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <Button size="sm" onClick={generate} disabled={busy || !hasTranscript || !hasModels}>
            {busy ? <Loader2 className="animate-spin" /> : <Sparkles />}
            {saved || streaming !== null ? "Regenerate" : "Summarize"}
          </Button>
        </div>
      </div>

      {/* Thinking card — pinned between header and summary, not in scroll */}
      <ThinkingDisplay
        key={generationKey}
        thinkingText={thinkingText}
        thinkingDuration={thinkingDuration}
        busy={busy}
      />

      {/* Loading spinner — before any tokens arrive */}
      {busy && thinkingText === null && streaming === "" && (
        <div className="flex items-center gap-1.5 border-b px-4 py-3 text-xs text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" />
          <span>Loading model…</span>
        </div>
      )}

      <ScrollArea className="min-h-0 flex-1" viewportRef={scrollRef}>
        <div className="p-4">
          {!hasModels && (
            <p className="text-xs text-muted-foreground">
              {isOllama
                ? "No Ollama models installed yet — pull one in Settings → Summaries."
                : "No built-in model downloaded — download one in Settings → Summaries."}
            </p>
          )}
          {error && <p className="pb-2 text-xs text-destructive">{error}</p>}

          {showText ? (
            <>
              <Markdown text={showText} />
              {streaming !== null && (
                <Loader2 className="mt-2 size-3.5 animate-spin text-muted-foreground" />
              )}
              {saved && streaming === null && (
                <p className="pt-3 text-[11px] text-muted-foreground/70">
                  {saved.model} · {new Date(saved.createdAtMs).toLocaleString()}
                </p>
              )}
            </>
          ) : (
            !error &&
            hasModels &&
            !busy && (
              <p className="text-xs text-muted-foreground">
                {hasTranscript
                  ? "Generate an on-device summary of this meeting."
                  : "Transcribe the meeting first, then summarize it."}
              </p>
            )
          )}
        </div>
      </ScrollArea>
    </div>
  );
}
