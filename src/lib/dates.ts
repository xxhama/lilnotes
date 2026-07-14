/**
 * Relative date formatting for due dates, shared across the Tasks surfaces.
 *
 * "Today" / "Tomorrow" / a weekday for this week / an absolute date for
 * anything further, and an "Overdue · …" prefix for past due dates (open
 * tasks only — callers gate the overdue prefix on status, this helper just
 * reports past/present/future).
 *
 * Uses the user's local timezone throughout. `null` → "No due date".
 */

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** Epoch ms at the start of today (local timezone). */
export function startOfTodayMs(): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Strip a time component so two dates compare on day boundaries. */
function atMidnight(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

export function isOverdue(dueAt: number | null): boolean {
  if (dueAt == null) return false;
  return atMidnight(dueAt) < startOfTodayMs();
}

/**
 * Render a due date. Returns `{ label, overdue }` so the caller controls the
 * destructive color (keeps this helper free of className concerns).
 */
export function fmtDueDate(dueAt: number | null): { label: string; overdue: boolean } {
  if (dueAt == null) return { label: "No due date", overdue: false };
  const today = startOfTodayMs();
  const due = atMidnight(dueAt);
  const dayMs = 86_400_000;
  const diffDays = Math.round((due - today) / dayMs);
  const overdue = diffDays < 0;

  const d = new Date(dueAt);
  let core: string;
  if (diffDays === 0) core = "Today";
  else if (diffDays === 1) core = "Tomorrow";
  else if (diffDays === -1) core = "Yesterday";
  else if (diffDays > 1 && diffDays < 7) core = WEEKDAYS[d.getDay()];
  else core = `${MONTHS[d.getMonth()]} ${d.getDate()}`;

  return { label: overdue ? `Overdue · ${core}` : core, overdue };
}
