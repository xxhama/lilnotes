import { useCallback, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, Loader2, RefreshCw, Sparkles } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  listOllamaModels,
  listSummaries,
  ollamaStatus,
  onSummaryToken,
  summarizeMeeting,
  type OllamaModels,
  type SummaryRow,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";

interface Props {
  meetingId: number;
  hasTranscript: boolean;
}

/** Minimal markdown rendering: headings + bullets; everything else as text. */
function Markdown({ text }: { text: string }) {
  return (
    <div className="space-y-1 text-sm leading-relaxed" data-selectable>
      {text.split("\n").map((line, i) => {
        if (line.startsWith("## ")) {
          return (
            <h3 key={i} className="pt-2 text-xs font-semibold tracking-wide text-muted-foreground uppercase">
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
 * saved summary, generates new ones token-by-token, and surfaces a setup
 * panel when Ollama isn't running.
 */
export default function SummaryPanel({ meetingId, hasTranscript }: Props) {
  const [reachable, setReachable] = useState<boolean | null>(null);
  const [models, setModels] = useState<OllamaModels | null>(null);
  const [model, setModel] = useState<string | null>(null);
  const [saved, setSaved] = useState<SummaryRow | null>(null);
  const [streaming, setStreaming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => {
    const status = await ollamaStatus();
    setReachable(status.reachable);
    if (status.reachable) {
      const m = await listOllamaModels().catch(() => null);
      setModels(m);
      setModel((prev) => prev ?? m?.active ?? null);
    }
    const rows = await listSummaries(meetingId).catch(() => []);
    setSaved(rows[0] ?? null);
  }, [meetingId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useTauriEvent(onSummaryToken, (e) => {
    if (e.meetingId !== meetingId) return;
    setStreaming((prev) => (prev ?? "") + e.token);
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  });

  const generate = useCallback(async () => {
    setBusy(true);
    setError(null);
    setStreaming("");
    try {
      const result = await summarizeMeeting(meetingId, model ?? undefined);
      setSaved({
        id: result.summaryId,
        model: result.model,
        content: result.content,
        createdAtMs: Date.now(),
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setStreaming(null);
      setBusy(false);
    }
  }, [meetingId, model]);

  // --- Ollama missing: friendly setup panel, not an error --------------
  if (reachable === false) {
    return (
      <div className="space-y-3 p-4">
        <h2 className="text-sm font-semibold">Summary</h2>
        <div className="space-y-3 rounded-lg border bg-card p-4 text-sm">
          <p className="font-medium">Ollama isn't running</p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            Summaries are generated fully on-device by a local model served
            by Ollama. Install it from ollama.com, launch it once, and come
            back — no account needed, nothing leaves this Mac.
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

  const showText = streaming !== null ? streaming : saved?.content;

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between gap-2 border-b p-4 pb-3">
        <h2 className="text-sm font-semibold">Summary</h2>
        <div className="flex items-center gap-2">
          {models && models.installed.length > 0 && (
            <select
              value={model ?? ""}
              onChange={(e) => setModel(e.target.value)}
              disabled={busy}
              className="h-7 max-w-40 rounded-md border bg-card px-1.5 text-xs outline-none focus:border-ring"
            >
              {models.installed.map((m) => (
                <option key={m.name} value={m.name}>
                  {m.name}
                </option>
              ))}
            </select>
          )}
          <Button
            size="sm"
            onClick={generate}
            disabled={busy || !hasTranscript || !models || models.installed.length === 0}
          >
            {busy ? <Loader2 className="animate-spin" /> : <Sparkles />}
            {saved || streaming !== null ? "Regenerate" : "Summarize"}
          </Button>
        </div>
      </div>

      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto p-4">
        {models && models.installed.length === 0 && (
          <p className="text-xs text-muted-foreground">
            No Ollama models installed yet — pull one in Settings → Summaries.
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
          models &&
          models.installed.length > 0 && (
            <p className="text-xs text-muted-foreground">
              {hasTranscript
                ? "Generate an on-device summary of this meeting."
                : "Transcribe the meeting first, then summarize it."}
            </p>
          )
        )}
      </div>
    </div>
  );
}
