import { cn } from "@/lib/utils";

// Deterministic palette for initial-letter avatars (no logo stored in v1).
const PALETTE = [
  "bg-rose-500/15 text-rose-700 dark:text-rose-300",
  "bg-amber-500/15 text-amber-700 dark:text-amber-300",
  "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  "bg-violet-500/15 text-violet-700 dark:text-violet-300",
  "bg-fuchsia-500/15 text-fuchsia-700 dark:text-fuchsia-300",
  "bg-cyan-500/15 text-cyan-700 dark:text-cyan-300",
  "bg-orange-500/15 text-orange-700 dark:text-orange-300",
];

interface Props {
  name: string;
  className?: string;
}

/** Colored initial-letter avatar (logo upload is deferred to v2). */
export default function CustomerAvatar({ name, className }: Props) {
  const letter = name.trim().charAt(0).toUpperCase() || "?";
  const color = PALETTE[name.charCodeAt(0) % PALETTE.length];
  return (
    <div
      className={cn(
        "flex shrink-0 items-center justify-center rounded-full font-semibold",
        color,
        className,
      )}
    >
      {letter}
    </div>
  );
}
