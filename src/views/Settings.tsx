import { useCallback, useEffect, useState } from "react";
import {
  CheckCircle2,
  Download,
  ExternalLink,
  HelpCircle,
  X,
  XCircle,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  cancelModelDownload,
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

import { useTauriEvent } from "@/lib/useTauriEvent";
import { cn } from "@/lib/utils";

type SysAudioState = "unknown" | "granted" | "denied";

function fmtBytes(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)} GB`;
  if (n >= 1_000_000) return `${Math.round(n / 1_000_000)} MB`;
  return `${Math.round(n / 1000)} kB`;
}

function StatusBadge({
  status,
}: {
  status: PermissionStatus | SysAudioState | null;
}) {
  if (status === "granted")
    return (
      <span className="inline-flex items-center gap-1 rounded-full bg-green-600/10 px-2 py-0.5 text-xs font-medium text-green-700 dark:text-green-400">
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
function Toggle({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
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
          "size-4.5 rounded-full bg-white shadow transition-transform",
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

  const refreshModels = useCallback(() => {
    listAsrModels().then(setAsrModels).catch(() => {});
  }, []);

  useEffect(() => {
    micPermissionStatus().then(setMicPerm);
    getSettings().then(setSettings);
    refreshModels();
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

  const probeSystem = useCallback(async () => {
    setProbing(true);
    try {
      await probeSystemAudioPermission();
      setSysAudio("granted");
    } catch {
      setSysAudio("denied");
    } finally {
      setProbing(false);
    }
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
    <div className="mx-auto max-w-2xl space-y-8 p-8 pt-12">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Settings</h1>
        <p className="text-sm text-muted-foreground">
          Transcription and permissions. Summary options land with milestone 6.
        </p>
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
                Transcribe in near-live chunks while recording. Off = transcribe
                after the meeting ends.
              </p>
            </div>
            {settings && (
              <Toggle
                checked={settings.liveTranscription}
                onChange={(v) => saveSettings({ ...settings, liveTranscription: v })}
              />
            )}
          </div>

          {asrModels.map((m) => {
            const p = progress[m.id];
            const downloading = p && !p.done;
            const pct =
              downloading && p.total ? Math.round((p.downloaded / p.total) * 100) : null;
            return (
              <div key={m.id} className="flex items-center gap-4 p-4">
                <input
                  type="radio"
                  name="asr-model"
                  className="size-4 accent-current"
                  checked={settings?.asrModel === m.id}
                  disabled={!m.downloaded}
                  onChange={() =>
                    settings && saveSettings({ ...settings, asrModel: m.id })
                  }
                />
                <div className="min-w-0 flex-1 space-y-0.5">
                  <div className="flex items-center gap-2 text-sm font-medium">
                    {m.label}
                    <span className="text-xs font-normal text-muted-foreground">
                      {fmtBytes(m.approxBytes)}
                    </span>
                    {m.downloaded && (
                      <span className="inline-flex items-center gap-1 rounded-full bg-green-600/10 px-2 py-0.5 text-[11px] font-medium text-green-700 dark:text-green-400">
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
        </div>
      </section>

      {/* ------------------------------------------------------------- */}
      {/* Storage                                                         */}
      {/* ------------------------------------------------------------- */}
      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">Storage</h2>

        <div className="divide-y rounded-xl border bg-card">
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
                Remove the WAV files once a meeting is transcribed and speakers
                are identified. Saves disk space; you can't re-transcribe.
              </p>
            </div>
            {settings && (
              <Toggle
                checked={settings.deleteAudioAfterTranscription}
                onChange={(v) =>
                  saveSettings({ ...settings, deleteAudioAfterTranscription: v })
                }
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
                Microphone <StatusBadge status={micPerm} />
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
                System audio{" "}
                <StatusBadge status={sysAudio === "unknown" ? null : sysAudio} />
              </div>
              <p className="text-xs text-muted-foreground">
                Other participants' voices. macOS calls this "System Audio
                Recording Only" — its status can only be checked by trying.
              </p>
            </div>
            <div className="flex shrink-0 gap-2">
              <Button size="sm" variant="outline" onClick={probeSystem} disabled={probing}>
                {probing ? "Testing…" : "Test access"}
              </Button>
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
        </div>
      </section>
    </div>
  );
}
