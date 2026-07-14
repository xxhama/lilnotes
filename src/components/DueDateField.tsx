/**
 * Due-date picker. The trigger button is the single source of truth — it shows
 * the *actual* state ("No due date" when empty, or "Today" / "Tomorrow" /
 * "Fri" / "Jul 14" when set), so the user is never fooled by a native date
 * input's greyed placeholder.
 *
 * The popover offers only **explicit actions, each of which sets-and-closes**:
 * quick presets (Today / Tomorrow / In a week), a themed shadcn Calendar for
 * any other date, and a Clear option. There is deliberately no "Done"/confirm
 * button — that created a trap where a date-looking empty field got confirmed
 * as nothing. Picking any option applies immediately and closes the popover.
 *
 * The Calendar is rendered from the app's CSS tokens (light + dark), not a
 * native WebKit control — native `<input type="date">` ignores the app theme
 * (the macOS native calendar dropdown stays light in dark mode), which is why
 * we use the shadcn Calendar instead.
 *
 * Reused by the customer page, global Tasks view, and meeting tab.
 */
import { useState } from "react";
import { Calendar as CalendarIcon, Check } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { fmtDueDate, startOfTodayMs } from "@/lib/dates";

interface Props {
  /** Epoch ms at local midnight, or null for "no due date". */
  value: number | null;
  onChange: (ms: number | null) => void;
  className?: string;
  ariaLabel?: string;
}

const DAY_MS = 86_400_000;

/** Normalise an epoch ms to local midnight (so calendar picks compare equal to
 * the preset timestamps, which are also local-midnight). */
function atLocalMidnight(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

export default function DueDateField({ value, onChange, className, ariaLabel }: Props) {
  const [open, setOpen] = useState(false);
  const { label, overdue } = fmtDueDate(value);
  const empty = value == null;

  // Presets are computed against the start of *today* (local) each time the
  // popover opens, so "Today" always means today.
  const today = startOfTodayMs();
  const presets = [
    { label: "Today", ms: today },
    { label: "Tomorrow", ms: today + DAY_MS },
    { label: "In a week", ms: today + 7 * DAY_MS },
  ];

  const choose = (ms: number | null) => {
    onChange(ms);
    setOpen(false);
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          className={cn(
            "h-8 w-40 shrink-0 justify-start gap-1.5 rounded-md bg-background text-sm font-normal",
            empty ? "text-muted-foreground" : overdue ? "text-destructive" : "text-foreground",
            className,
          )}
          aria-label={ariaLabel ?? "Due date"}
        >
          <CalendarIcon className="size-3.5 shrink-0" />
          <span className="truncate">{empty ? "No due date" : label}</span>
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-auto p-1.5">
        <div className="flex flex-col">
          {presets.map((p) => {
            const active = value === p.ms;
            return (
              <button
                key={p.label}
                type="button"
                onClick={() => choose(p.ms)}
                className="flex items-center justify-between rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent"
              >
                <span>{p.label}</span>
                {active && <Check className="size-4 text-muted-foreground" />}
              </button>
            );
          })}

          <div className="my-1 h-px bg-border" />

          {/* Themed calendar (renders from CSS tokens, light + dark). Picking
           * a day applies it immediately and closes — no separate confirm. */}
          <Calendar
            mode="single"
            selected={value != null ? new Date(value) : undefined}
            defaultMonth={value != null ? new Date(value) : new Date()}
            onSelect={(d) => choose(d ? atLocalMidnight(d.getTime()) : null)}
            aria-label="Pick a due date"
          />

          {!empty && (
            <>
              <div className="my-1 h-px bg-border" />
              <button
                type="button"
                onClick={() => choose(null)}
                className="rounded-md px-2 py-1.5 text-left text-sm text-muted-foreground transition-colors hover:bg-accent"
              >
                Clear due date
              </button>
            </>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
