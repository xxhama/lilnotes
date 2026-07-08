/**
 * Settings view.
 *
 * Later milestones add: transcription model picker (M3), summary model +
 * template and the Ollama model manager (M6), storage location and
 * delete-audio toggle (M5), permission status (M2).
 */
export default function SettingsView() {
  return (
    <div className="flex h-full items-center justify-center p-8">
      <p className="text-sm text-muted-foreground">
        Settings — options appear as features land in milestones 2–6.
      </p>
    </div>
  );
}
