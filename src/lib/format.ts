// Token/cost formatting + fixed UI labels (plan AC5 wording).

export const LABEL_ESTIMATED = "추정";
export const LABEL_MEASURED = "로그 기준";
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
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
