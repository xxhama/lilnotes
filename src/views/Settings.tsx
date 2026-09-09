/**
 * Settings view. Tabs for transcription model management (download with
 * progress, select active model, live transcription toggle), summary
 * settings (Ollama connection status, model picker, prompt template
 * editor), storage (recordings folder, delete-after-transcription), and
 * personas/voiceprints management.
 */
import { useCallback, useEffect, useState } from "react";
import {
  Bug,
  Check,
  CheckCircle2,
  Copy,
  Download,
  ExternalLink,
  Eye,
  EyeOff,
  FileText,
  Github,
  HelpCircle,
  RefreshCw,
  Scale,
  X,
  XCircle,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  cancelModelDownload,
  deleteAllVoiceprints,
  downloadAsrModel,
  getSettings,
  listAsrModels,
  mcpStatus,
  micPermissionStatus,
  onAudioMigrationProgress,
  onModelProgress,
  openPrivacySettings,
  probeSystemAudioPermission,
  regenerateMcpToken,
  requestMicPermission,
  updateSettings,
  type AecAggressiveness,
  type AppSettings,
  type AsrModelInfo,
  type AudioMigrationProgress,
  type DownloadProgress,
  type McpStatus,
  type PermissionStatus,
} from "@/lib/ipc";
import { getVersion } from "@tauri-apps/api/app";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

import NativeModelManager from "@/components/NativeModelManager";
import OllamaManager from "@/components/OllamaManager";
import { defaultSummaryTemplate } from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";
import { cn } from "@/lib/utils";

type SysAudioState = "unknown" | "granted" | "denied";

const REPO_URL = "https://github.com/xxhama/lilnotes";

function fmtBytes(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)} GB`;
  if (n >= 1_000_000) return `${Math.round(n / 1_000_000)} MB`;
  return `${Math.round(n / 1000)} kB`;
}

function StatusBadge({ status }: { status: PermissionStatus | SysAudioState | null }) {
  if (status === "granted")
    return (
      <span className="inline-flex items-center gap-1 rounded-full bg-success/10 px-2 py-0.5 text-xs font-medium text-success">
        <CheckCircle2 className="size-3" /> Granted
      </span>
    );
  if (status === "denied" || status === "restricted")
    return (
      <span className="inline-flex items-center gap-1 rounded-full bg-destructive/10 px-2 py-0.5 text-xs font-medium text-destructive">
        <XCircle className="size-3" /> Denied
      </span>
    );
  return (
    <span className="inline-flex items-center gap-1 rounded-full bg-secondary px-2 py-0.5 text-xs font-medium text-muted-foreground">
      <HelpCircle className="size-3" /> Not determined
    </span>
  );
}

/** Themed toggle backed by the shadcn Switch primitive (checked/onCheckedChange). */
function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return <Switch checked={checked} onCheckedChange={onChange} />;
}

export default function SettingsView() {
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [noticesOpen, setNoticesOpen] = useState(false);
  const [notices, setNotices] = useState<string | null>(null);

  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion(null));
  }, []);

  // The notices file is ~600 kB; load it only when the dialog is opened.
  useEffect(() => {
    if (!noticesOpen || notices !== null) return;
    import("../../THIRD_PARTY_NOTICES.md?raw")
      .then((m) => setNotices(m.default))
      .catch(() => setNotices("Could not load THIRD_PARTY_NOTICES.md."));
  }, [noticesOpen, notices]);

  const [micPerm, setMicPerm] = useState<PermissionStatus | null>(null);
  const [sysAudio, setSysAudio] = useState<SysAudioState>("unknown");
  const [probing, setProbing] = useState(false);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [asrModels, setAsrModels] = useState<AsrModelInfo[]>([]);
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});
  const [templatePlaceholder, setTemplatePlaceholder] = useState("");
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [showToken, setShowToken] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const [portDraft, setPortDraft] = useState<string | null>(null);

  const refreshMcp = useCallback(() => {
    mcpStatus()
      .then(setMcp)
      .catch(() => {});
  }, []);

  const refreshModels = useCallback(() => {
    listAsrModels()
      .then(setAsrModels)
      .catch(() => {});
  }, []);

  useEffect(() => {
    micPermissionStatus().then(setMicPerm);
    getSettings().then(setSettings);
    defaultSummaryTemplate().then(setTemplatePlaceholder);
    refreshModels();
    refreshMcp();
    // Auto-probe system audio access. There's no status-query API, so the
    // probe (create + destroy a process tap) is the only way to check. On
    // first use it surfaces the TCC prompt automatically.
    (async () => {
      setProbing(true);
      try {
        await probeSystemAudioPermission();
        setSysAudio("granted");
      } catch {
        setSysAudio("denied");
      } finally {
        setProbing(false);
      }
    })();
  }, [refreshModels, refreshMcp]);

  // While the MCP server is enabled, poll its status so a port conflict or
  // a crash shows up without a reload (no event exists for it).
  useEffect(() => {
    if (!settings?.mcpEnabled) return;
    const id = setInterval(refreshMcp, 5000);
    return () => clearInterval(id);
  }, [settings?.mcpEnabled, refreshMcp]);

  /** One-time WAV → FLAC conversion of pre-0.3 recordings (runs in the
   * background after launch). Null until the backend reports anything. */
  const [audioMigration, setAudioMigration] = useState<AudioMigrationProgress | null>(null);
  useTauriEvent(onAudioMigrationProgress, setAudioMigration);

  useTauriEvent(onModelProgress, (p) => {
    setProgress((prev) => ({ ...prev, [p.id]: p }));
    if (p.done) refreshModels();
  });

  const saveSettings = useCallback(
    async (next: AppSettings) => {
      setSettings(next);
      // The backend may normalize (e.g. generate the MCP token on first
      // enable) — adopt what it stored.
      const stored = await updateSettings(next);
      setSettings(stored);
      refreshModels(); // "active" flags depend on settings
      refreshMcp();
    },
    [refreshModels, refreshMcp],
  );

  const copy = useCallback(async (key: string, text: string) => {
    try {
      await writeText(text);
      setCopied(key);
      setTimeout(() => setCopied((c) => (c === key ? null : c)), 1500);
    } catch {
      /* clipboard unavailable — text stays selectable */
    }
  }, []);

  const commitPort = useCallback(() => {
    if (!settings || portDraft === null) return;
    const n = parseInt(portDraft, 10);
    setPortDraft(null);
    if (!Number.isFinite(n) || n < 1024 || n > 65535 || n === settings.mcpPort) return;
    saveSettings({ ...settings, mcpPort: n });
  }, [settings, portDraft, saveSettings]);

  const requestMic = useCallback(async () => {
    const granted = await requestMicPermission();
    setMicPerm(granted ? "granted" : "denied");
  }, []);

  const startDownload = useCallback(
    (id: string) => {
      setProgress((prev) => ({
        ...prev,
        [id]: { id, downloaded: 0, total: null, done: false, error: null },
      }));
      downloadAsrModel(id)
        .catch(() => {})
        .finally(refreshModels);
    },
    [refreshModels],
  );

  return (
    <div className="mx-auto max-w-2xl space-y-8 p-8 pt-4">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Settings</h1>
        <p className="text-sm text-muted-foreground">Transcription, summaries, and permissions.</p>
      </div>

      {/* ------------------------------------------------------------- */}
      {/* Transcription                                                   */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">Transcription</h2>

        <div className="divide-y rounded-xl border bg-card">
          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Live transcription</div>
              <p className="text-xs text-muted-foreground">
                Transcribe in near-live chunks while recording. Off = transcribe after the meeting
                ends.
              </p>
            </div>
            {settings && (
              <Toggle
                checked={settings.liveTranscription}
                onChange={(v) => saveSettings({ ...settings, liveTranscription: v })}
              />
            )}
          </div>
        </div>

        <RadioGroup
          value={settings?.asrModel}
          onValueChange={(v) => settings && saveSettings({ ...settings, asrModel: v })}
          className="grid gap-0 divide-y rounded-xl border bg-card"
        >
          {asrModels.map((m) => {
            const p = progress[m.id];
            const downloading = p && !p.done;
            const pct = downloading && p.total ? Math.round((p.downloaded / p.total) * 100) : null;
            return (
              <div key={m.id} className="flex items-center gap-4 p-4">
                <RadioGroupItem value={m.id} disabled={!m.downloaded} />
                <div className="min-w-0 flex-1 space-y-0.5">
                  <div className="flex items-center gap-2 text-sm font-medium">
                    {m.label}
                    <span className="text-xs font-normal text-muted-foreground">
                      {fmtBytes(m.approxBytes)}
                    </span>
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
                      <span className="w-16 text-right text-[11px] tabular-nums text-muted-foreground">
                        {pct !== null ? `${pct}%` : fmtBytes(p.downloaded)}
                      </span>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="size-6"
                        onClick={() => cancelModelDownload(m.id)}
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
      </section>

      {/* ------------------------------------------------------------- */}
      {/* Summaries                                                        */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">Summaries</h2>

        {/* Backend selector + model manager (single card) */}
        {settings && (
          <div className="divide-y rounded-xl border bg-card">
            <div className="p-4 space-y-2">
              <div className="text-sm font-medium">Backend</div>
              <div className="flex gap-2">
                <button
                  onClick={() => saveSettings({ ...settings, summaryBackend: "native" })}
                  className={cn(
                    "flex-1 rounded-lg border p-3 text-left transition-colors",
                    settings.summaryBackend !== "ollama"
                      ? "border-primary bg-primary/5"
                      : "border-border hover:border-ring",
                  )}
                >
                  <div className="flex items-center gap-2 text-sm font-medium">
                    {settings.summaryBackend !== "ollama" && (
                      <CheckCircle2 className="size-4 text-primary" />
                    )}
                    Built-in
                    <span className="rounded-full bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium text-primary">
                      Recommended
                    </span>
                  </div>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Runs entirely on your Mac. No setup needed.
                  </p>
                </button>
                <button
                  onClick={() => saveSettings({ ...settings, summaryBackend: "ollama" })}
                  className={cn(
                    "flex-1 rounded-lg border p-3 text-left transition-colors",
                    settings.summaryBackend === "ollama"
                      ? "border-primary bg-primary/5"
                      : "border-border hover:border-ring",
                  )}
                >
                  <div className="flex items-center gap-2 text-sm font-medium">
                    {settings.summaryBackend === "ollama" && (
                      <CheckCircle2 className="size-4 text-primary" />
                    )}
                    Ollama
                    <span className="rounded-full bg-secondary px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
                      Advanced
                    </span>
                  </div>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Use a locally running Ollama instance with any model.
                  </p>
                </button>
              </div>
            </div>
            {settings.summaryBackend === "ollama" ? (
              <OllamaManager settings={settings} onSave={saveSettings} />
            ) : (
              <NativeModelManager settings={settings} onSave={saveSettings} />
            )}
          </div>
        )}

        {/* Template editor */}
        <div className="space-y-2 rounded-xl border bg-card p-4">
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Prompt template</div>
              <p className="text-xs text-muted-foreground">
                {"{title}"} and {"{transcript}"} are filled in automatically.
              </p>
            </div>
            {settings?.summaryTemplate && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => settings && saveSettings({ ...settings, summaryTemplate: null })}
              >
                Reset to default
              </Button>
            )}
          </div>
          <Textarea
            value={settings?.summaryTemplate ?? templatePlaceholder}
            onChange={(e) =>
              settings && setSettings({ ...settings, summaryTemplate: e.target.value })
            }
            onBlur={() => {
              if (!settings) return;
              const v = settings.summaryTemplate?.trim();
              saveSettings({
                ...settings,
                summaryTemplate: !v || v === templatePlaceholder.trim() ? null : v,
              });
            }}
            rows={8}
            spellCheck={false}
            className="w-full resize-y rounded-md bg-background px-2 py-2 font-mono text-xs leading-relaxed"
            data-selectable
          />
        </div>
      </section>

      {/* ------------------------------------------------------------- */}
      {/* MCP server (AI agents)                                          */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">MCP server (AI agents)</h2>

        <div className="divide-y rounded-xl border bg-card">
          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Enable MCP server</div>
              <p className="text-xs text-muted-foreground">
                Lets local AI agents such as Claude Code read your meetings, transcripts, summaries,
                notes, customers and personas over the Model Context Protocol. Localhost only,
                token-protected, read-only.
              </p>
            </div>
            {settings && (
              <Toggle
                checked={settings.mcpEnabled}
                onChange={(v) => saveSettings({ ...settings, mcpEnabled: v })}
              />
            )}
          </div>

          <div className={cn("space-y-4 p-4", !settings?.mcpEnabled && "opacity-60")}>
            <div className="flex items-center justify-between gap-4">
              <div className="space-y-0.5">
                <div className="text-sm font-medium">Status</div>
                <p className="text-xs text-muted-foreground">
                  {mcp?.error
                    ? "The server could not start — pick another port or free this one."
                    : mcp?.running
                      ? "Agents can connect while LilNotes is running (also when hidden in the menu bar)."
                      : "Turn the server on to accept connections."}
                </p>
              </div>
              {mcp?.error ? (
                <span className="inline-flex max-w-[50%] items-center gap-1 rounded-full bg-destructive/10 px-2 py-0.5 text-xs font-medium text-destructive">
                  <XCircle className="size-3 shrink-0" />
                  <span className="truncate" title={mcp.error}>
                    {mcp.error}
                  </span>
                </span>
              ) : mcp?.running ? (
                <span className="inline-flex items-center gap-1 rounded-full bg-success/10 px-2 py-0.5 text-xs font-medium text-success">
                  <CheckCircle2 className="size-3" /> Running · {mcp.url}
                </span>
              ) : (
                <span className="inline-flex items-center gap-1 rounded-full bg-secondary px-2 py-0.5 text-xs font-medium text-muted-foreground">
                  <HelpCircle className="size-3" /> Stopped
                </span>
              )}
            </div>

            <label className="flex items-center justify-between gap-3 text-xs">
              <span>
                Port <span className="text-muted-foreground">(1024–65535, loopback only)</span>
              </span>
              <Input
                type="number"
                min="1024"
                max="65535"
                step="1"
                value={portDraft ?? settings?.mcpPort ?? 41777}
                disabled={!settings?.mcpEnabled}
                onChange={(e) => setPortDraft(e.target.value)}
                onBlur={commitPort}
                onKeyDown={(e) => {
                  if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                }}
                className="h-7 w-24 rounded-md bg-background px-2 text-right"
              />
            </label>

            <div className="space-y-1.5">
              <div className="flex items-center justify-between gap-3 text-xs">
                <span>Bearer token</span>
                <div className="flex gap-1">
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 px-2"
                    disabled={!settings?.mcpToken}
                    onClick={() => setShowToken((v) => !v)}
                    title={showToken ? "Hide token" : "Reveal token"}
                  >
                    {showToken ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 px-2"
                    disabled={!settings?.mcpToken}
                    onClick={() => settings?.mcpToken && copy("token", settings.mcpToken)}
                    title="Copy token"
                  >
                    {copied === "token" ? (
                      <Check className="size-3.5" />
                    ) : (
                      <Copy className="size-3.5" />
                    )}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7"
                    disabled={!settings?.mcpEnabled}
                    onClick={async () => {
                      if (
                        !confirm(
                          "Regenerate the MCP token? Every configured agent must be updated.",
                        )
                      )
                        return;
                      const stored = await regenerateMcpToken();
                      setSettings(stored);
                      refreshMcp();
                    }}
                  >
                    <RefreshCw className="size-3.5" /> Regenerate
                  </Button>
                </div>
              </div>
              <code
                className="block truncate rounded-md bg-background px-2 py-1.5 font-mono text-xs"
                data-selectable
              >
                {settings?.mcpToken
                  ? showToken
                    ? settings.mcpToken
                    : `${"•".repeat(24)}…${settings.mcpToken.slice(-4)}`
                  : "Generated when you enable the server"}
              </code>
            </div>

            <div className="space-y-1.5">
              <div className="flex items-center justify-between gap-3 text-xs">
                <span>Connect Claude Code</span>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 px-2"
                  disabled={!settings?.mcpToken}
                  onClick={() =>
                    settings?.mcpToken &&
                    copy(
                      "claude",
                      `claude mcp add --transport http lilnotes ${mcp?.url ?? `http://127.0.0.1:${settings.mcpPort}/mcp`} --header "Authorization: Bearer ${settings.mcpToken}"`,
                    )
                  }
                  title="Copy command"
                >
                  {copied === "claude" ? (
                    <Check className="size-3.5" />
                  ) : (
                    <Copy className="size-3.5" />
                  )}
                </Button>
              </div>
              <pre
                className="overflow-x-auto rounded-md bg-background px-2 py-1.5 font-mono text-xs leading-relaxed"
                data-selectable
              >
                {`claude mcp add --transport http lilnotes ${mcp?.url ?? `http://127.0.0.1:${settings?.mcpPort ?? 41777}/mcp`} --header "Authorization: Bearer ${settings?.mcpToken ? (showToken ? settings.mcpToken : "<token>") : "<token>"}"`}
              </pre>
            </div>

            <div className="space-y-1.5">
              <div className="flex items-center justify-between gap-3 text-xs">
                <span>Other clients (Cursor, generic MCP config)</span>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 px-2"
                  disabled={!settings?.mcpToken}
                  onClick={() =>
                    settings?.mcpToken &&
                    copy(
                      "json",
                      JSON.stringify(
                        {
                          mcpServers: {
                            lilnotes: {
                              url: mcp?.url ?? `http://127.0.0.1:${settings.mcpPort}/mcp`,
                              headers: { Authorization: `Bearer ${settings.mcpToken}` },
                            },
                          },
                        },
                        null,
                        2,
                      ),
                    )
                  }
                  title="Copy JSON"
                >
                  {copied === "json" ? (
                    <Check className="size-3.5" />
                  ) : (
                    <Copy className="size-3.5" />
                  )}
                </Button>
              </div>
              <pre
                className="overflow-x-auto rounded-md bg-background px-2 py-1.5 font-mono text-xs leading-relaxed"
                data-selectable
              >
                {JSON.stringify(
                  {
                    mcpServers: {
                      lilnotes: {
                        url: mcp?.url ?? `http://127.0.0.1:${settings?.mcpPort ?? 41777}/mcp`,
                        headers: {
                          Authorization: `Bearer ${settings?.mcpToken && showToken ? settings.mcpToken : "<token>"}`,
                        },
                      },
                    },
                  },
                  null,
                  2,
                )}
              </pre>
            </div>

            <p className="text-xs text-muted-foreground">
              Off by default. Binds to 127.0.0.1 only — nothing leaves your Mac. Audio files, app
              settings and voiceprints are never exposed; all tools are read-only.
            </p>
          </div>
        </div>
      </section>

      {/* ------------------------------------------------------------- */}
      {/* Personas & voiceprints                                          */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">Personas &amp; voiceprints</h2>

        <div className="divide-y rounded-xl border bg-card">
          <div className="space-y-2 p-4">
            <div className="text-sm font-medium">Match thresholds</div>
            <p className="text-xs text-muted-foreground">
              Cosine similarity cutoffs for matching known personas. Higher = fewer false matches.
            </p>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Auto-suggest (pre-fill)</span>
              <Input
                type="number"
                step="0.01"
                min="0"
                max="1"
                value={settings?.personaAutoThreshold ?? 0.65}
                onChange={(e) =>
                  settings &&
                  saveSettings({
                    ...settings,
                    personaAutoThreshold: parseFloat(e.target.value) || 0,
                  })
                }
                className="h-7 w-20 rounded-md bg-background px-2 text-right"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Tentative suggestion</span>
              <Input
                type="number"
                step="0.01"
                min="0"
                max="1"
                value={settings?.personaSuggestThreshold ?? 0.45}
                onChange={(e) =>
                  settings &&
                  saveSettings({
                    ...settings,
                    personaSuggestThreshold: parseFloat(e.target.value) || 0,
                  })
                }
                className="h-7 w-20 rounded-md bg-background px-2 text-right"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Live auto-identify</span>
              <Input
                type="number"
                step="0.01"
                min="0"
                max="1"
                value={settings?.personaLiveThreshold ?? 0.72}
                onChange={(e) =>
                  settings &&
                  saveSettings({
                    ...settings,
                    personaLiveThreshold: parseFloat(e.target.value) || 0,
                  })
                }
                className="h-7 w-20 rounded-md bg-background px-2 text-right"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Voiceprints per persona (cap)</span>
              <Input
                type="number"
                step="1"
                min="0"
                value={settings?.voiceprintGalleryCap ?? 50}
                onChange={(e) =>
                  settings &&
                  saveSettings({
                    ...settings,
                    voiceprintGalleryCap: parseInt(e.target.value, 10) || 0,
                  })
                }
                className="h-7 w-20 rounded-md bg-background px-2 text-right"
              />
            </label>
          </div>

          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Delete all voiceprints</div>
              <p className="text-xs text-muted-foreground">
                Clears every persona's stored voiceprints. Personas stay, but recognition starts
                over. Manage individual personas in the Personas view.
              </p>
            </div>
            <Button
              size="sm"
              variant="outline"
              onClick={async () => {
                if (!confirm("Delete ALL stored voiceprints?")) return;
                await deleteAllVoiceprints();
              }}
            >
              Clear
            </Button>
          </div>

          <div className="p-4 text-xs text-muted-foreground">
            Voiceprints are biometric data stored only in the local SQLite database at{" "}
            <code className="rounded bg-secondary px-1">
              ~/Library/Application Support/co.elastic.lilnote/lilnotes.sqlite3
            </code>
            . They never leave your Mac.
          </div>
        </div>
      </section>

      {/* ------------------------------------------------------------- */}
      {/* Recording                                                       */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">Recording</h2>

        <div className="divide-y rounded-xl border bg-card">
          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Echo cancellation</div>
              <p className="text-xs text-muted-foreground">
                Cancels speaker echo from the mic using the system-audio feed as a reference (WebRTC
                AEC3 + noise suppression). On by default. Disable if you use headphones — there's no
                echo to cancel, and this reverts to Apple's voice processing.
              </p>
            </div>
            {settings && (
              <Toggle
                checked={settings.aecEnabled}
                onChange={(v) => saveSettings({ ...settings, aecEnabled: v })}
              />
            )}
          </div>

          <div className={cn("space-y-3 p-4", !settings?.aecEnabled && "opacity-60")}>
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Echo suppression</div>
              <p className="text-xs text-muted-foreground">
                Stronger removes more speaker echo from your mic (the loud-speakers / quiet-room
                case). May slightly dull your own voice at Maximum.
              </p>
            </div>
            <RadioGroup
              value={settings?.aecAggressiveness}
              onValueChange={(v) =>
                settings && saveSettings({ ...settings, aecAggressiveness: v as AecAggressiveness })
              }
              disabled={!settings?.aecEnabled}
              className="grid grid-cols-3 gap-2"
            >
              {(["balanced", "strong", "maximum"] as const).map((lvl) => (
                <label
                  key={lvl}
                  className={cn(
                    "flex items-center justify-center gap-2 rounded-md border border-input bg-card px-3 py-2 text-sm capitalize shadow-xs transition-[color,box-shadow]",
                    settings?.aecAggressiveness === lvl &&
                      "border-ring text-foreground ring-1 ring-ring/40",
                    !settings?.aecEnabled && "cursor-not-allowed",
                  )}
                >
                  <RadioGroupItem value={lvl} disabled={!settings?.aecEnabled} />
                  {lvl}
                </label>
              ))}
            </RadioGroup>
          </div>

          <div className="flex items-center justify-between gap-4 p-4">
            <div className="min-w-0 space-y-0.5">
              <div className="text-sm font-medium">Recordings location</div>
              <p className="truncate text-xs text-muted-foreground" data-selectable>
                {settings?.storageDir ?? "Default (app data folder)"}
              </p>
              {audioMigration && audioMigration.done < audioMigration.total && (
                <p className="text-xs text-muted-foreground">
                  Converting older recordings to FLAC… {audioMigration.done}/{audioMigration.total}
                </p>
              )}
              {audioMigration &&
                audioMigration.done === audioMigration.total &&
                audioMigration.failed > 0 && (
                  <p className="text-xs text-muted-foreground">
                    {audioMigration.failed} older recording
                    {audioMigration.failed === 1 ? "" : "s"} could not be converted to FLAC and stay
                    as WAV.
                  </p>
                )}
            </div>
            <div className="flex shrink-0 gap-2">
              <Button
                size="sm"
                variant="outline"
                onClick={async () => {
                  if (!settings) return;
                  const dir = await openDialog({ directory: true, multiple: false });
                  if (typeof dir === "string") {
                    saveSettings({ ...settings, storageDir: dir });
                  }
                }}
              >
                Choose…
              </Button>
              {settings?.storageDir && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => saveSettings({ ...settings, storageDir: null })}
                >
                  Reset
                </Button>
              )}
            </div>
          </div>

          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Delete audio after transcription</div>
              <p className="text-xs text-muted-foreground">
                Remove the audio files once a meeting is transcribed and speakers are identified.
                Saves disk space; you can't re-transcribe.
              </p>
            </div>
            {settings && (
              <Toggle
                checked={settings.deleteAudioAfterTranscription}
                onChange={(v) => saveSettings({ ...settings, deleteAudioAfterTranscription: v })}
              />
            )}
          </div>
        </div>
      </section>

      {/* ------------------------------------------------------------- */}
      {/* Permissions                                                     */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">Permissions</h2>

        <div className="divide-y rounded-xl border bg-card">
          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="flex items-center gap-2 text-sm font-medium">
                <span className="w-24 shrink-0">Microphone</span>
                <StatusBadge status={micPerm} />
              </div>
              <p className="text-xs text-muted-foreground">
                Your side of the meeting, recorded to its own track.
              </p>
            </div>
            <div className="flex shrink-0 gap-2">
              {micPerm === "undetermined" && (
                <Button size="sm" onClick={requestMic}>
                  Request access
                </Button>
              )}
              {(micPerm === "denied" || micPerm === "restricted") && (
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => openPrivacySettings("microphone")}
                >
                  <ExternalLink /> System Settings
                </Button>
              )}
            </div>
          </div>

          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="flex items-center gap-2 text-sm font-medium">
                <span className="w-24 shrink-0">System audio</span>
                {probing ? (
                  <span className="inline-flex items-center gap-1 rounded-full bg-secondary px-2 py-0.5 text-xs font-medium text-muted-foreground">
                    Checking…
                  </span>
                ) : (
                  <StatusBadge status={sysAudio === "unknown" ? null : sysAudio} />
                )}
              </div>
              <p className="text-xs text-muted-foreground">
                Other participants' voices (Zoom, Teams, etc.). macOS calls this "System Audio
                Recording Only."
              </p>
            </div>
            {sysAudio === "denied" && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => openPrivacySettings("systemAudio")}
              >
                <ExternalLink /> System Settings
              </Button>
            )}
          </div>
        </div>
      </section>

      {/* ------------------------------------------------------------- */}
      {/* About                                                           */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">About</h2>

        <div className="divide-y rounded-xl border bg-card">
          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">
                LilNotes{appVersion ? ` ${appVersion}` : ""}
              </div>
              <p className="text-xs text-muted-foreground">
                Free software under the GNU AGPL v3. Everything stays on this Mac.
              </p>
            </div>
            <div className="flex shrink-0 flex-wrap justify-end gap-2">
              <Button size="sm" variant="outline" onClick={() => openUrl(REPO_URL)}>
                <Github /> Source code
              </Button>
              <Button size="sm" variant="outline" onClick={() => openUrl(`${REPO_URL}/issues`)}>
                <Bug /> Report an issue
              </Button>
            </div>
          </div>

          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="text-sm font-medium">Licenses</div>
              <p className="text-xs text-muted-foreground">
                Built on whisper.cpp, llama.cpp, sherpa-onnx, ONNX Runtime, WebRTC and others.
              </p>
            </div>
            <div className="flex shrink-0 flex-wrap justify-end gap-2">
              <Button
                size="sm"
                variant="outline"
                onClick={() => openUrl(`${REPO_URL}/blob/main/LICENSE`)}
              >
                <Scale /> AGPL-3.0
              </Button>
              <Button size="sm" variant="outline" onClick={() => setNoticesOpen(true)}>
                <FileText /> Third-party notices
              </Button>
            </div>
          </div>
        </div>
      </section>

      <Dialog open={noticesOpen} onOpenChange={setNoticesOpen}>
        <DialogContent className="flex max-h-[80vh] flex-col sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>Third-party notices</DialogTitle>
            <DialogDescription>
              Licenses of the open-source components LilNotes is built from.
            </DialogDescription>
          </DialogHeader>
          <pre className="min-h-0 flex-1 overflow-auto rounded-md border bg-muted/40 p-3 text-xs whitespace-pre-wrap">
            {notices ?? "Loading…"}
          </pre>
        </DialogContent>
      </Dialog>
    </div>
  );
}
