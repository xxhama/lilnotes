import { useCallback, useEffect, useState } from "react";
import { CheckCircle2, ExternalLink, HelpCircle, XCircle } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  micPermissionStatus,
  openPrivacySettings,
  probeSystemAudioPermission,
  requestMicPermission,
  type PermissionStatus,
} from "@/lib/ipc";

type SysAudioState = "unknown" | "granted" | "denied";

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

/**
 * Settings view.
 *
 * M2: permission status. Later milestones add the transcription model picker
 * (M3), summary model/template + Ollama model manager (M6), storage location
 * and delete-audio toggle (M5).
 */
export default function SettingsView() {
  const [micPerm, setMicPerm] = useState<PermissionStatus | null>(null);
  const [sysAudio, setSysAudio] = useState<SysAudioState>("unknown");
  const [probing, setProbing] = useState(false);

  useEffect(() => {
    micPermissionStatus().then(setMicPerm);
  }, []);

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

  return (
    <div className="mx-auto max-w-2xl space-y-8 p-8 pt-12">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Settings</h1>
        <p className="text-sm text-muted-foreground">
          Recording permissions. More options land with milestones 3–6.
        </p>
      </div>

      <section className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground">Permissions</h2>

        <div className="divide-y rounded-xl border bg-card">
          {/* Microphone */}
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

          {/* System audio */}
          <div className="flex items-center justify-between gap-4 p-4">
            <div className="space-y-0.5">
              <div className="flex items-center gap-2 text-sm font-medium">
                System audio <StatusBadge status={sysAudio === "unknown" ? null : sysAudio} />
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
