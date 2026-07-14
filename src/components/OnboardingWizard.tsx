import { useCallback, useEffect, useRef, useState } from "react";
import {
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Download,
  ExternalLink,
  Loader2,
  Lock,
  Mic,
  Volume2,
  X,
  XCircle,
  Zap,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  cancelModelDownload,
  cancelNativeModelDownload,
  downloadAsrModel,
  downloadNativeModel,
  getSettings,
  initDb,
  listAsrModels,
  listNativeModels,
  micPermissionStatus,
  onModelProgress,
  openPrivacySettings,
  probeSystemAudioPermission,
  requestMicPermission,
  updateSettings,
  type AppSettings,
  type AsrModelInfo,
  type DownloadProgress,
  type NativeLlmModelInfo,
  type PermissionStatus,
} from "@/lib/ipc";
import { useTauriEvent } from "@/lib/useTauriEvent";
import { cn } from "@/lib/utils";

type Step = "welcome" | "security" | "permissions" | "models";
type SysAudioState = "unknown" | "granted" | "denied";

const STEPS: Step[] = ["welcome", "security", "permissions", "models"];

function fmtBytes(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)} GB`;
  if (n >= 1_000_000) return `${Math.round(n / 1_000_000)} MB`;
  return `${Math.round(n / 1000)} kB`;
}

interface Props {
  onComplete: () => void;
}

/**
 * First-launch onboarding wizard. Walks through secure storage setup
 * (Keychain), permissions (mic + system audio), and model downloads
 * (ASR + summary). Shown when the DB file doesn't exist (new user).
 */
export default function OnboardingWizard({ onComplete }: Props) {
  const [step, setStep] = useState<Step>("welcome");
  const [micPerm, setMicPerm] = useState<PermissionStatus | null>(null);
  const [sysAudio, setSysAudio] = useState<SysAudioState>("unknown");
  const [probing, setProbing] = useState(false);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [asrModels, setAsrModels] = useState<AsrModelInfo[]>([]);
  const [nativeModels, setNativeModels] = useState<NativeLlmModelInfo[]>([]);
  const [selectedAsrId, setSelectedAsrId] = useState<string | null>(null);
  const [selectedLlmId, setSelectedLlmId] = useState<string | null>(null);
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});

  // Security step state — DB init is deferred to the "Set up secure storage"
  // button so the Keychain prompt fires during onboarding, not at launch.
  const [dbReady, setDbReady] = useState(false);
  const [initializing, setInitializing] = useState(false);
  const [initError, setInitError] = useState<string | null>(null);

  // Guards the auto-probe effect against React StrictMode's dev double-mount
  // (probing state alone can't: setProbing(true) isn't committed between the
  // two invokes, so a state guard would fire the probe twice and race two
  // AudioHardwareCreateProcessTap calls). Mirrors Recording.tsx startInFlightRef.
  const probeFiredRef = useRef(false);

  const refreshModels = useCallback(async () => {
    setAsrModels(await listAsrModels().catch(() => []));
    setNativeModels(await listNativeModels().catch(() => []));
  }, []);

  // Load only mic permission on mount — no DB access yet.
  useEffect(() => {
    micPermissionStatus()
      .then(setMicPerm)
      .catch(() => null);
  }, []);

  useTauriEvent(onModelProgress, (p) => {
    setProgress((prev) => ({ ...prev, [p.id]: p }));
    if (p.done) {
      refreshModels();
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

  // Auto-finish when both models are already downloaded.
  useEffect(() => {
    if (step === "models" && selectedAsrId && selectedLlmId) {
      const asr = asrModels.find((m) => m.id === selectedAsrId);
      const llm = nativeModels.find((m) => m.id === selectedLlmId);
      if (asr?.downloaded && llm?.downloaded) finishOnboarding();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step, selectedAsrId, selectedLlmId, asrModels, nativeModels]);

  // --- Actions -----------------------------------------------------------
  const handleInitDb = useCallback(async () => {
    setInitializing(true);
    setInitError(null);
    try {
      await initDb();
      setDbReady(true);
      const s = await getSettings().catch(() => null);
      setSettings(s);
      await refreshModels();
      setStep("permissions");
    } catch (e) {
      setInitError(String(e));
    } finally {
      setInitializing(false);
    }
  }, [refreshModels]);

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

  // Auto-probe system audio on the permissions step, sequenced after mic is
  // granted. The probe IS the TCC request — it surfaces the "System Audio
  // Recording" prompt during onboarding (instead of mid-first-record). The
  // ref guards against React StrictMode's dev double-mount (probing state
  // alone can't: setProbing(true) isn't committed between the two invokes).
  useEffect(() => {
    if (step !== "permissions") return;
    if (micPerm !== "granted") return;
    if (sysAudio !== "unknown") return;
    if (probing) return;
    if (probeFiredRef.current) return;
    probeFiredRef.current = true;
    void probeSystem();
  }, [step, micPerm, sysAudio, probing, probeSystem]);

  const startAsrDownload = useCallback(
    (id: string) => {
      setSelectedAsrId(id);
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

  const startLlmDownload = useCallback(
    (id: string) => {
      setSelectedLlmId(id);
      setProgress((prev) => ({
        ...prev,
        [id]: { id, downloaded: 0, total: null, done: false, error: null },
      }));
      downloadNativeModel(id)
        .catch(() => {})
        .finally(refreshModels);
    },
    [refreshModels],
  );

  function finishOnboarding() {
    if (!settings) return;
    updateSettings({
      ...settings,
      onboardingComplete: true,
      asrModel: selectedAsrId ?? settings.asrModel,
      summaryModel: selectedLlmId ?? settings.summaryModel,
    }).then(() => onComplete());
  }

  // --- Step index for indicator -----------------------------------------
  const stepIndex = STEPS.indexOf(step);

  const asrReady = !!selectedAsrId && !!asrModels.find((m) => m.id === selectedAsrId)?.downloaded;
  const llmReady =
    !!selectedLlmId && !!nativeModels.find((m) => m.id === selectedLlmId)?.downloaded;

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-8 pt-4">
      {/* Step indicator */}
      <div className="flex items-center gap-2">
        {STEPS.map((s, i) => (
          <div
            key={s}
            className={cn(
              "h-1.5 flex-1 rounded-full transition-colors",
              i <= stepIndex ? "bg-primary" : "bg-secondary",
            )}
          />
        ))}
      </div>

      {/* ----------------------------------------------------------------- */}
      {/* Welcome */}
      {/* ----------------------------------------------------------------- */}
      {step === "welcome" && (
        <div className="space-y-6 rounded-xl border bg-card p-8 text-center">
          <div className="mx-auto flex size-16 items-center justify-center rounded-2xl bg-primary/10">
            <Mic className="size-8 text-primary" />
          </div>
          <div className="space-y-2">
            <h1 className="text-xl font-semibold tracking-tight">Welcome to LilNotes</h1>
            <p className="text-sm leading-relaxed text-muted-foreground">
              Record meetings with automatic transcription, speaker identification, and AI-generated
              summaries — all on-device, nothing leaves your Mac. Let's get set up in a few steps.
            </p>
          </div>
          <Button size="lg" className="w-full" onClick={() => setStep("security")}>
            Get Started
          </Button>
        </div>
      )}

      {/* ----------------------------------------------------------------- */}
      {/* Security — Keychain / encrypted storage */}
      {/* ----------------------------------------------------------------- */}
      {step === "security" && (
        <div className="space-y-4 rounded-xl border bg-card p-6">
          <div className="space-y-1">
            <h2 className="text-sm font-medium">Secure storage</h2>
            <p className="text-xs leading-relaxed text-muted-foreground">
              LilNotes stores all your meeting data — transcripts, voiceprints, and summaries — in
              an encrypted database. The encryption key is kept in macOS Keychain, so only your Mac
              can read your data.
            </p>
          </div>

          {dbReady ? (
            <div className="flex items-center gap-2 text-sm text-success">
              <CheckCircle2 className="size-4" /> Secure storage enabled
            </div>
          ) : initError ? (
            <div className="space-y-3">
              <div className="flex items-center gap-2 text-sm text-destructive">
                <XCircle className="size-4" /> {initError}
              </div>
              <Button size="sm" variant="outline" onClick={handleInitDb} disabled={initializing}>
                Try again
              </Button>
            </div>
          ) : (
            <Button size="sm" onClick={handleInitDb} disabled={initializing}>
              {initializing ? (
                <>
                  <Loader2 className="animate-spin" /> Setting up…
                </>
              ) : (
                <>
                  <Lock /> Set up secure storage
                </>
              )}
            </Button>
          )}

          <NavButtons
            onBack={() => setStep("welcome")}
            onNext={() => dbReady && setStep("permissions")}
            nextDisabled={!dbReady}
          />
        </div>
      )}

      {/* ----------------------------------------------------------------- */}
      {/* Permissions — mic + system audio */}
      {/* ----------------------------------------------------------------- */}
      {step === "permissions" && (
        <div className="space-y-4 rounded-xl border bg-card p-6">
          <div className="space-y-1">
            <h2 className="text-sm font-medium">Permissions</h2>
            <p className="text-xs leading-relaxed text-muted-foreground">
              LilNotes needs microphone access to record your side of the meeting, and system audio
              access to capture other participants from Zoom, Teams, etc.
            </p>
          </div>

          {/* Microphone row */}
          <div className="space-y-2 border-t pt-4">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Mic className="size-4" />
              Microphone
            </div>
            <p className="text-xs text-muted-foreground">
              Required for recording your side of the meeting.
            </p>
            {micPerm === "granted" ? (
              <div className="flex items-center gap-2 text-sm text-success">
                <CheckCircle2 className="size-4" /> Microphone access granted
              </div>
            ) : micPerm === "denied" || micPerm === "restricted" ? (
              <div className="space-y-2">
                <div className="flex items-center gap-2 text-sm text-destructive">
                  <XCircle className="size-4" /> Microphone access denied
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => openPrivacySettings("microphone")}
                >
                  <ExternalLink /> Open System Settings
                </Button>
              </div>
            ) : (
              <Button size="sm" onClick={requestMic}>
                <Mic /> Request access
              </Button>
            )}
          </div>

          {/* System audio row */}
          <div className="space-y-2 border-t pt-4">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Volume2 className="size-4" />
              System audio
            </div>
            <p className="text-xs text-muted-foreground">
              To capture other participants' voices, LilNotes needs "System Audio Recording"
              permission. macOS only allows checking this by briefly creating an audio tap.
            </p>
            {sysAudio === "granted" ? (
              <div className="flex items-center gap-2 text-sm text-success">
                <CheckCircle2 className="size-4" /> System audio access granted
              </div>
            ) : sysAudio === "denied" ? (
              <div className="space-y-2">
                <div className="flex items-center gap-2 text-sm text-destructive">
                  <XCircle className="size-4" /> System audio access denied
                </div>
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => openPrivacySettings("systemAudio")}
                  >
                    <ExternalLink /> Open System Settings
                  </Button>
                  <Button size="sm" onClick={probeSystem} disabled={probing}>
                    {probing ? (
                      <>
                        <Loader2 className="animate-spin" /> Testing…
                      </>
                    ) : (
                      "Test again"
                    )}
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground">
                  macOS won&apos;t re-prompt after a denial. Enable LilNotes in System Settings,
                  then test again.
                </p>
              </div>
            ) : (
              <Button size="sm" onClick={probeSystem} disabled={probing}>
                {probing ? (
                  <>
                    <Loader2 className="animate-spin" /> Testing…
                  </>
                ) : (
                  "Test access"
                )}
              </Button>
            )}
          </div>

          <NavButtons
            onBack={() => setStep("security")}
            onNext={() => setStep("models")}
            nextDisabled={micPerm !== "granted" || sysAudio !== "granted"}
          />
        </div>
      )}

      {/* ----------------------------------------------------------------- */}
      {/* Models — ASR + summary */}
      {/* ----------------------------------------------------------------- */}
      {step === "models" && (
        <div className="space-y-4">
          {/* Transcription model */}
          <div className="space-y-1 px-1">
            <h2 className="text-sm font-medium">Download a transcription model</h2>
            <p className="text-xs leading-relaxed text-muted-foreground">
              Used to transcribe your recordings into text. Pick one — you can always change it
              later in Settings.
            </p>
          </div>

          <RadioGroup
            value={selectedAsrId ?? undefined}
            onValueChange={setSelectedAsrId}
            className="grid gap-0 divide-y rounded-xl border bg-card"
          >
            {asrModels.map((m) => (
              <ModelCard
                key={m.id}
                id={m.id}
                label={m.label}
                approxBytes={m.approxBytes}
                note={m.note}
                downloaded={m.downloaded}
                progress={progress[m.id]}
                recommended={m.id === "large-v3-turbo"}
                onDownload={() => startAsrDownload(m.id)}
                onCancel={() => cancelModelDownload(m.id)}
              />
            ))}
          </RadioGroup>

          {/* Summary model */}
          <div className="space-y-1 px-1 pt-2">
            <h2 className="text-sm font-medium">Download a summary model</h2>
            <p className="text-xs leading-relaxed text-muted-foreground">
              Used to generate meeting summaries. Runs on-device with Metal acceleration. Pick one.
            </p>
          </div>

          <RadioGroup
            value={selectedLlmId ?? undefined}
            onValueChange={setSelectedLlmId}
            className="grid gap-0 divide-y rounded-xl border bg-card"
          >
            {nativeModels.map((m) => (
              <ModelCard
                key={m.id}
                id={m.id}
                label={m.label}
                approxBytes={m.approxBytes}
                note={m.note}
                downloaded={m.downloaded}
                progress={progress[m.id]}
                recommended={m.id === "qwen3.5-4b"}
                onDownload={() => startLlmDownload(m.id)}
                onCancel={() => cancelNativeModelDownload(m.id)}
              />
            ))}
          </RadioGroup>

          <NavButtons
            onBack={() => setStep("permissions")}
            onNext={finishOnboarding}
            nextLabel="Finish"
            nextDisabled={!asrReady || !llmReady}
          />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Shared sub-components
// ---------------------------------------------------------------------------

function NavButtons({
  onBack,
  onNext,
  nextLabel = "Next",
  nextDisabled = false,
}: {
  onBack: () => void;
  onNext: () => void;
  nextLabel?: string;
  nextDisabled?: boolean;
}) {
  return (
    <div className="flex items-center justify-between pt-2">
      <Button size="sm" variant="ghost" onClick={onBack}>
        <ChevronLeft /> Back
      </Button>
      <Button size="sm" onClick={onNext} disabled={nextDisabled}>
        {nextLabel}
        {nextLabel === "Finish" ? null : <ChevronRight />}
      </Button>
    </div>
  );
}

function ModelCard({
  id,
  label,
  approxBytes,
  note,
  downloaded,
  progress,
  recommended,
  onDownload,
  onCancel,
}: {
  id: string;
  label: string;
  approxBytes: number;
  note: string;
  downloaded: boolean;
  progress?: DownloadProgress;
  recommended?: boolean;
  onDownload: () => void;
  onCancel: () => void;
}) {
  const downloading = progress && !progress.done;
  const pct =
    downloading && progress.total ? Math.round((progress.downloaded / progress.total) * 100) : null;

  return (
    <div className="flex items-center gap-4 p-4">
      <RadioGroupItem value={id} disabled={!downloaded} />
      <div className="min-w-0 flex-1 space-y-0.5">
        <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
          <span data-selectable>{label}</span>
          <span className="text-xs font-normal text-muted-foreground">
            ~{fmtBytes(approxBytes)}
          </span>
          {recommended && (
            <span className="inline-flex items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-medium text-primary">
              <Zap className="size-3" /> recommended
            </span>
          )}
          {downloaded && (
            <span className="inline-flex items-center gap-1 rounded-full bg-success/10 px-2 py-0.5 text-[11px] font-medium text-success">
              <CheckCircle2 className="size-3" /> Downloaded
            </span>
          )}
        </div>
        <p className="text-xs text-muted-foreground">{note}</p>
        {downloading && (
          <div className="flex items-center gap-2 pt-1">
            <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-secondary">
              <div
                className="h-full rounded-full bg-primary transition-[width]"
                style={{ width: `${pct ?? 5}%` }}
              />
            </div>
            <span className="w-24 text-right text-[11px] tabular-nums text-muted-foreground">
              {pct !== null ? `${pct}% · ${fmtBytes(progress.downloaded)}` : "starting…"}
            </span>
            <Button
              size="icon"
              variant="ghost"
              className="size-6"
              onClick={onCancel}
              aria-label="Cancel download"
            >
              <X className="size-3.5" />
            </Button>
          </div>
        )}
        {progress?.error && progress.error !== "cancelled" && (
          <p className="pt-1 text-xs text-destructive">{progress.error}</p>
        )}
      </div>
      {!downloaded && !downloading && (
        <Button size="sm" variant="outline" onClick={onDownload}>
          <Download /> Download
        </Button>
      )}
    </div>
  );
}
