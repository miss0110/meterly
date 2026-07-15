// Token/cost formatting + fixed UI labels (plan AC5 wording).

export const LABEL_ESTIMATED = "추정";
export const LABEL_MEASURED = "실시간";
export const LABEL_CLI = "실시간";
export const LABEL_COST = "API 환산";
export const LABEL_COST_NA = "N/A";
export const LABEL_READ_ERROR = "⚠ 읽기오류";

export function formatTokens(n: number): string {
  if (n < 1_000) return String(n);
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)}K`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  return `${(n / 1_000_000_000).toFixed(1)}B`;
}

export function formatCost(usd: number | null | undefined): string {
  if (usd === null || usd === undefined) return LABEL_COST_NA;
  return `$${usd.toFixed(usd < 10 ? 2 : 1)}`;
}

export function formatResetTime(iso: string): string {
  return formatResetDate(new Date(iso));
}

/** Format a Date to the viewer's locale — time only if today, else date + time.
 *  `[]` locale = the OS locale, i.e. the user's country's standard format. */
function formatResetDate(d: Date): string {
  const now = new Date();
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  // A weekly reset can be days out — show the date unless it's today.
  if (d.toDateString() === now.toDateString()) return time;
  return `${d.toLocaleDateString([], { month: "numeric", day: "numeric" })} ${time}`;
}

const MONTHS = ["jan","feb","mar","apr","may","jun","jul","aug","sep","oct","nov","dec"];

/** Claude's `/usage` prints resets as an English string in the user's own
 *  timezone, e.g. "Jul 19 at 8:59pm (Asia/Seoul)" — inconsistent with Codex's
 *  locale-formatted times. Parse it to a Date (year inferred; a reset that
 *  already passed rolls to next year) and re-format it the same way. Falls back
 *  to the original string if it doesn't match the expected shape. */
export function formatResetLabel(label: string | null): string | null {
  if (!label) return label;
  const m = label.match(
    /([A-Za-z]{3})\s+(\d{1,2})\s+at\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)/i,
  );
  if (!m) return label;
  const monthIdx = MONTHS.indexOf(m[1].toLowerCase());
  if (monthIdx < 0) return label;
  const day = parseInt(m[2], 10);
  let hour = parseInt(m[3], 10) % 12;
  if (/pm/i.test(m[5])) hour += 12;
  const min = m[4] ? parseInt(m[4], 10) : 0;
  const now = new Date();
  // Claude prints in local time, so build a local-time Date directly.
  let d = new Date(now.getFullYear(), monthIdx, day, hour, min);
  if (d.getTime() < now.getTime() - 31 * 86_400_000) {
    d = new Date(now.getFullYear() + 1, monthIdx, day, hour, min);
  }
  return Number.isNaN(d.getTime()) ? label : formatResetDate(d);
}

/** Label a rate-limit window by its length (Codex reports `window_minutes`;
 *  300 = 5h session, 10080 = 7d weekly). */
export function windowLabel(minutes: number): string {
  if (minutes === 300) return "세션";
  if (minutes === 10080) return "주간";
  if (minutes < 1440) return `${Math.round(minutes / 60)}시간`;
  return `${Math.round(minutes / 1440)}일`;
}
