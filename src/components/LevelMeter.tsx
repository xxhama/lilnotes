import { cn } from "@/lib/utils";

interface Props {
  label: string;
  /** Linear RMS level 0..1 */
  rms: number;
  /** Linear peak level 0..1 */
  peak: number;
  className?: string;
}

/** Map a linear level to a 0..1 meter position on a -60..0 dB scale. */
function toMeter(level: number): number {
  if (level <= 0.000001) return 0;
  const db = 20 * Math.log10(level);
  return Math.min(1, Math.max(0, (db + 60) / 60));
}

export default function LevelMeter({ label, rms, peak, className }: Props) {
  const rmsPos = toMeter(rms);
  const peakPos = toMeter(peak);
  const clipping = peak >= 0.99;

  return (
    <div className={cn("flex items-center gap-3", className)}>
      <span className="w-16 shrink-0 text-right text-xs font-medium text-muted-foreground">
        {label}
      </span>
      <div className="relative h-2.5 flex-1 overflow-hidden rounded-full bg-secondary">
        {/* RMS fill */}
        <div
          className={cn(
            "h-full rounded-full transition-[width] duration-100 ease-out",
            clipping ? "bg-recording" : "bg-primary/70",
          )}
          style={{ width: `${rmsPos * 100}%` }}
        />
        {/* Peak tick */}
        {peakPos > 0.01 && (
          <div
            className={cn(
              "absolute top-0 h-full w-0.5 transition-[left] duration-100 ease-out",
              clipping ? "bg-recording" : "bg-foreground/60",
            )}
            style={{ left: `${peakPos * 100}%` }}
          />
        )}
      </div>
    </div>
  );
}
