import * as React from "react";
import { Check, Minus } from "lucide-react";

import { cn } from "@/lib/utils";

/**
 * Accessible checkbox built on a native button with ARIA + keyboard support
 * (no Radix dependency). The app avoids native `<input type="checkbox">` because
 * macOS WebKit renders those inconsistently across theme/system settings —
 * a styled button keeps both light and dark themes pixel-stable.
 *
 * `checked` is controlled; `indeterminate` shows a dash (used by "select all"
 * patterns). onClick fires for Space/Enter as well as click.
 */
function Checkbox({
  className,
  checked = false,
  indeterminate = false,
  onCheckedChange,
  ...props
}: Omit<React.ComponentProps<"button">, "onChange" | "value"> & {
  checked?: boolean;
  indeterminate?: boolean;
  onCheckedChange?: (checked: boolean) => void;
}) {
  const isOn = checked || indeterminate;
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={indeterminate ? "mixed" : checked}
      data-slot="checkbox"
      onClick={(e) => {
        e.stopPropagation();
        onCheckedChange?.(!checked);
        props.onClick?.(e);
      }}
      className={cn(
        "peer size-4 shrink-0 rounded-[4px] border border-input shadow-xs transition-colors outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:border-ring disabled:cursor-not-allowed disabled:opacity-50",
        isOn
          ? "bg-primary border-primary text-primary-foreground"
          : "bg-background hover:bg-accent",
        className,
      )}
      {...props}
    >
      {indeterminate ? (
        <Minus className="size-3.5" strokeWidth={3} />
      ) : checked ? (
        <Check className="size-3.5" strokeWidth={3} />
      ) : null}
    </button>
  );
}

export { Checkbox };
