import { useCallback, useEffect, useState } from "react";
import {
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Download,
  ExternalLink,
  Loader2,
  Mic,
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

type Step = "welcome" | "mic" | "system-audio" | "asr" | "llm";
type SysAudioState = "unknown" | "granted" | "denied";

const STEPS: Step[] = ["welcome", "mic", "system-audio", "asr", "llm"];

function fmtBytes(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)} GB`;
  if (n >= 1_000_000) return `${Math.round(n / 1_000_000)} MB`;
  return `${Math.round(n / 1000)} kB`;
}

interface Props {
  onComplete: () => void;
}

/**
 * First-launch onboarding wizard. Walks through mic permission, system-audio
 * probe, ASR model download, and LLM model download. Shown when
 * `settings.onboardingComplete` is false.
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

  const refreshModels = useCallback(async () => {
    setAsrModels(await listAsrModels().catch(() => []));
    setNativeModels(await listNativeModels().catch(() => []));
  }, []);

  useEffect(() => {
    (async () => {
      const s = await getSettings().catch(() => null);
      setSettings(s);
      const perm = await micPermissionStatus().catch(() => null);
      setMicPerm(perm);
      await refreshModels();
    })();
  }, [refreshModels]);

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

  // --- Auto-advance for already-satisfied steps ---------------------------
  useEffect(() => {
    if (step === "mic" && micPerm === "granted") {
      setStep("system-audio");
    }
  }, [step, micPerm]);

  useEffect(() => {
    if (step === "asr" && selectedAsrId) {
      const model = asrModels.find((m) => m.id === selectedAsrId);
      if (model?.downloaded) setStep("llm");
    }
  }, [step, selectedAsrId, asrModels]);

  useEffect(() => {
    if (step === "llm" && selectedLlmId) {
      const model = nativeModels.find((m) => m.id === selectedLlmId);
      if (model?.downloaded) finishOnboarding();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step, selectedLlmId, nativeModels]);

  // --- Actions -----------------------------------------------------------
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

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-8 pt-6">
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
          <Button size="lg" className="w-full" onClick={() => setStep("mic")}>
            Get Started
          </Button>
        </div>
      )}

      {/* ----------------------------------------------------------------- */}
      {/* Microphone permission */}
      {/* ----------------------------------------------------------------- */}
      {step === "mic" && (
        <div className="space-y-4 rounded-xl border bg-card p-6">
          <div className="space-y-1">
            <h2 className="text-sm font-medium">Microphone access</h2>
            <p className="text-xs leading-relaxed text-muted-foreground">
              LilNotes records your side of the meeting on its own track. macOS requires your
              permission to use the microphone.
            </p>
          </div>

          {micPerm === "granted" ? (
            <div className="flex items-center gap-2 text-sm text-green-700 dark:text-green-400">
              <CheckCircle2 className="size-4" /> Microphone access granted
            </div>
          ) : micPerm === "denied" || micPerm === "restricted" ? (
            <div className="space-y-3">
              <div className="flex items-center gap-2 text-sm text-destructive">
                <XCircle className="size-4" /> Microphone access denied
              </div>
              <p className="text-xs text-muted-foreground">
                Enable microphone access in System Settings, then come back.
              </p>
              <Button size="sm" variant="outline" onClick={() => openPrivacySettings("microphone")}>
                <ExternalLink /> Open System Settings
              </Button>
            </div>
          ) : (
            <Button size="sm" onClick={requestMic}>
              <Mic /> Request access
            </Button>
          )}

          <NavButtons
            onBack={() => setStep("welcome")}
            onNext={() => micPerm === "granted" && setStep("system-audio")}
            nextDisabled={micPerm !== "granted"}
          />
        </div>
      )}

      {/* ----------------------------------------------------------------- */}
      {/* System audio permission */}
      {/* ----------------------------------------------------------------- */}
      {step === "system-audio" && (
        <div className="space-y-4 rounded-xl border bg-card p-6">
          <div className="space-y-1">
            <h2 className="text-sm font-medium">System audio access</h2>
            <p className="text-xs leading-relaxed text-muted-foreground">
              To capture other participants' voices (from Zoom, Teams, etc.), LilNotes needs "System
              Audio Recording" permission. macOS only allows checking this by briefly creating an
              audio tap.
            </p>
          </div>

          {sysAudio === "granted" ? (
            <div className="flex items-center gap-2 text-sm text-green-700 dark:text-green-400">
              <CheckCircle2 className="size-4" /> System audio access granted
            </div>
          ) : sysAudio === "denied" ? (
            <div className="space-y-3">
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
                <Button size="sm" variant="ghost" onClick={() => setStep("asr")}>
                  Skip
                </Button>
              </div>
            </div>
          ) : (
            <div className="flex gap-2">
              <Button size="sm" onClick={probeSystem} disabled={probing}>
                {probing ? (
                  <>
                    <Loader2 className="animate-spin" /> Testing…
                  </>
                ) : (
                  "Test access"
                )}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setStep("asr")}>
                Skip
              </Button>
            </div>
          )}

          <NavButtons
            onBack={() => setStep("mic")}
            onNext={() => setStep("asr")}
            nextLabel="Next"
          />
        </div>
      )}

      {/* ----------------------------------------------------------------- */}
      {/* ASR model download */}
      {/* ----------------------------------------------------------------- */}
      {step === "asr" && (
        <div className="space-y-4">
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

          <NavButtons
            onBack={() => setStep("system-audio")}
            onNext={() => {
              const model = asrModels.find((m) => m.id === selectedAsrId);
              if (model?.downloaded) setStep("llm");
            }}
            nextLabel="Next"
            nextDisabled={
              !selectedAsrId || !asrModels.find((m) => m.id === selectedAsrId)?.downloaded
            }
          />
        </div>
      )}

      {/* ----------------------------------------------------------------- */}
      {/* LLM model download */}
      {/* ----------------------------------------------------------------- */}
      {step === "llm" && (
        <div className="space-y-4">
          <div className="space-y-1 px-1">
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
            onBack={() => setStep("asr")}
            onNext={finishOnboarding}
            nextLabel="Finish"
            nextDisabled={
              !selectedLlmId || !nativeModels.find((m) => m.id === selectedLlmId)?.downloaded
            }
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
            <span className="inline-flex items-center gap-1 rounded-full bg-green-600/10 px-2 py-0.5 text-[11px] font-medium text-green-700 dark:text-green-400">
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
