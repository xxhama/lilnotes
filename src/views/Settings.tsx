/**
 * Settings view. Tabs for transcription model management (download with
 * progress, select active model, live transcription toggle), summary
 * settings (Ollama connection status, model picker, prompt template
 * editor), storage (recordings folder, delete-after-transcription), and
 * personas/voiceprints management.
 */
import { useCallback, useEffect, useState } from "react";
import { CheckCircle2, Download, ExternalLink, HelpCircle, X, XCircle } from "lucide-react";

import { Button } from "@/components/ui/button";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  cancelModelDownload,
  deleteAllVoiceprints,
  downloadAsrModel,
  getSettings,
  listAsrModels,
  micPermissionStatus,
  onModelProgress,
  openPrivacySettings,
  probeSystemAudioPermission,
  requestMicPermission,
  updateSettings,
  type AppSettings,
  type AsrModelInfo,
  type DownloadProgress,
  type PermissionStatus,
} from "@/lib/ipc";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import NativeModelManager from "@/components/NativeModelManager";
import OllamaManager from "@/components/OllamaManager";
import { defaultSummaryTemplate } from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";
import { cn } from "@/lib/utils";

type SysAudioState = "unknown" | "granted" | "denied";

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

/** Simple styled toggle (no extra Radix dep needed yet). */
function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={cn(
        "inline-flex h-6 w-10 shrink-0 items-center rounded-full transition-colors",
        checked ? "bg-primary" : "bg-input",
      )}
    >
      <span
        className={cn(
          "size-4.5 rounded-full bg-primary-foreground shadow transition-transform",
          checked ? "translate-x-[1.125rem]" : "translate-x-[0.1875rem]",
        )}
      />
    </button>
  );
}

export default function SettingsView() {
  const [micPerm, setMicPerm] = useState<PermissionStatus | null>(null);
  const [sysAudio, setSysAudio] = useState<SysAudioState>("unknown");
  const [probing, setProbing] = useState(false);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [asrModels, setAsrModels] = useState<AsrModelInfo[]>([]);
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});
  const [templatePlaceholder, setTemplatePlaceholder] = useState("");

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
  }, [refreshModels]);

  useTauriEvent(onModelProgress, (p) => {
    setProgress((prev) => ({ ...prev, [p.id]: p }));
    if (p.done) refreshModels();
  });

  const saveSettings = useCallback(
    async (next: AppSettings) => {
      setSettings(next);
      await updateSettings(next);
      refreshModels(); // "active" flags depend on settings
    },
    [refreshModels],
  );

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
          <textarea
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
            className="w-full resize-y rounded-md border bg-background p-2 font-mono text-xs leading-relaxed outline-none focus:border-ring"
            data-selectable
          />
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
              <input
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
                className="h-7 w-20 rounded-md border bg-background px-2 text-right outline-none focus:border-ring"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Tentative suggestion</span>
              <input
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
                className="h-7 w-20 rounded-md border bg-background px-2 text-right outline-none focus:border-ring"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Live auto-identify</span>
              <input
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
                className="h-7 w-20 rounded-md border bg-background px-2 text-right outline-none focus:border-ring"
              />
            </label>
            <label className="flex items-center justify-between gap-3 text-xs">
              <span>Voiceprints per persona (cap)</span>
              <input
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
                className="h-7 w-20 rounded-md border bg-background px-2 text-right outline-none focus:border-ring"
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

          <div className="flex items-center justify-between gap-4 p-4">
            <div className="min-w-0 space-y-0.5">
              <div className="text-sm font-medium">Recordings location</div>
              <p className="truncate text-xs text-muted-foreground" data-selectable>
                {settings?.storageDir ?? "Default (app data folder)"}
              </p>
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
                Remove the WAV files once a meeting is transcribed and speakers are identified.
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
    </div>
  );
}
