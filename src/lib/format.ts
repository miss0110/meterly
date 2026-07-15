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
  const d = new Date(iso);
  const now = new Date();
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  // A weekly reset can be days out — show the date unless it's today.
  if (d.toDateString() === now.toDateString()) return time;
  return `${d.toLocaleDateString([], { month: "numeric", day: "numeric" })} ${time}`;
}

/** Label a rate-limit window by its length (Codex reports `window_minutes`;
 *  300 = 5h session, 10080 = 7d weekly). */
export function windowLabel(minutes: number): string {
  if (minutes === 300) return "세션";
  if (minutes === 10080) return "주간";
  if (minutes < 1440) return `${Math.round(minutes / 60)}시간`;
  return `${Math.round(minutes / 1440)}일`;
}
