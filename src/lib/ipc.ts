// Typed IPC wrappers for the fixed command contract (plan: Contract surface).
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type SourceId = "claude_code" | "codex";

export type SourceHealth =
  | "ok"
  | { partial: { skipped_lines: number; note: string } }
  | { error: { reason: string } };

export type RateLimitStatus =
  | "unavailable"
  | {
      estimated: {
        window_hours: number;
        window_tokens: number;
        window_started: string;
        resets_at: string;
      };
    }
  | {
      measured: {
        primary_used_percent: number;
        secondary_used_percent: number | null;
        window_minutes: number;
        resets_at: string;
        secondary_resets_at: string | null;
      };
    }
  | {
      cli: {
        session_percent: number | null;
        windows: UsageWindow[];
      };
    };

/** One window from the real `claude -p "/usage"` readout. */
export interface UsageWindow {
  label: string;
  used_percent: number;
  resets_label: string | null;
}

export interface TokenBreakdown {
  input: number;
  output: number;
  cache_read: number;
  cache_creation: number;
  total: number;
}

export interface SourceSummary {
  id: SourceId;
  display_name: string;
  health: SourceHealth;
  today_tokens: TokenBreakdown;
  today_cost_usd: number | null;
  /** USD saved today by cache reads vs full input rate (known models). */
  today_cache_saved_usd: number | null;
  rate_limit: RateLimitStatus;
  /** Daily totals, oldest → today (7 entries) — sparklines. */
  last7_totals: number[];
}

export interface Summary {
  generated_at: string;
  sources: SourceSummary[];
}

export interface DashboardRow {
  period: string;
  source: SourceId;
  model: string | null;
  tokens: TokenBreakdown;
  cost_usd: number | null;
}

export interface DashboardData {
  range: string;
  rows: DashboardRow[];
  timezone_note: string;
}

export interface HeatmapCell {
  /** 0 = 월 … 6 = 일 (로컬 tz). */
  weekday: number;
  hour: number;
  total: number;
}

export type Range = "daily30" | "weekly12" | "monthly6";

export const getSummary = () => invoke<Summary>("get_summary");
export const getDashboard = (range: Range) =>
  invoke<DashboardData>("get_dashboard", { range });
export const refreshNow = () => invoke<Summary | null>("refresh_now");
export const getHeatmap = () => invoke<HeatmapCell[]>("get_heatmap");
export const exportData = (range: Range, format: "csv" | "json") =>
  invoke<string>("export_data", { range, format });
export const openDashboard = () => invoke<void>("open_dashboard");

export const onUsageUpdated = (
  handler: (summary: Summary) => void,
): Promise<UnlistenFn> =>
  listen<Summary>("usage-updated", (event) => handler(event.payload));
