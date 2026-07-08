import { useState } from "react";
import { CheckCircle2, Mic, XCircle } from "lucide-react";

import { Button } from "@/components/ui/button";
import { ping, type PingResponse } from "@/lib/ipc";
import type { Route } from "@/App";

interface Props {
  onNavigate: (route: Route) => void;
}

/**
 * Home / meeting history.
 *
 * Milestone 1: empty state + backend IPC round-trip check.
 * Milestone 5 will add the searchable meeting list backed by SQLite.
 */
export default function HomeView({ onNavigate }: Props) {
  const [result, setResult] = useState<PingResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [latencyMs, setLatencyMs] = useState<number | null>(null);

  async function runPing() {
    setResult(null);
    setError(null);
    const t0 = performance.now();
    try {
      const res = await ping("hello from the webview");
      setLatencyMs(Math.round((performance.now() - t0) * 10) / 10);
      setResult(res);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col items-center justify-center gap-6 p-8 text-center">
      <div className="flex size-14 items-center justify-center rounded-2xl bg-secondary">
        <Mic className="size-6 text-muted-foreground" />
      </div>
      <div className="space-y-1.5">
        <h1 className="text-xl font-semibold tracking-tight">No meetings yet</h1>
        <p className="text-sm text-muted-foreground">
          Record your first meeting to see it here. Everything stays on this
          Mac.
        </p>
      </div>
      <div className="flex gap-3">
        <Button onClick={() => onNavigate({ name: "recording" })}>
          <Mic /> Start recording
        </Button>
        <Button variant="outline" onClick={runPing}>
          Test backend connection
        </Button>
      </div>

      {result && (
        <div className="flex items-center gap-2 rounded-lg border bg-card px-4 py-2.5 text-sm">
          <CheckCircle2 className="size-4 text-green-600" />
          <span data-selectable>
            Backend v{result.version} replied in {latencyMs} ms —{" "}
            <span className="text-muted-foreground">“{result.echo}”</span>
          </span>
        </div>
      )}
      {error && (
        <div className="flex items-center gap-2 rounded-lg border border-destructive/30 bg-card px-4 py-2.5 text-sm text-destructive">
          <XCircle className="size-4" />
          <span data-selectable>{error}</span>
        </div>
      )}
    </div>
  );
}
