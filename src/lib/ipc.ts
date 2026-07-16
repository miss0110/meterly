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
  /** Logged-in account this source measures (e.g. "email · Team"). */
  account: string | null;
}

export interface Summary {
  generated_at: string;
  sources: SourceSummary[];
}

export interface DeviceSourceUsage {
  id: SourceId;
  display_name: string;
  today_tokens: TokenBreakdown;
  today_cost_usd: number | null;
  /** USD saved today by cache reads (known models) — 전체/host views. */
  today_cache_saved_usd: number | null;
  /** Daily totals, oldest → today (7 entries) — sparkline. */
  last7_totals: number[];
}

export interface DeviceSummary {
  device_id: string;
  hostname: string;
  updated_at: string;
  is_current: boolean;
  sources: DeviceSourceUsage[];
}

export interface DevicesData {
  sync_enabled: boolean;
  devices: DeviceSummary[];
}

export interface DashboardRow {
  period: string;
  source: SourceId;
  model: string | null;
  tokens: TokenBreakdown;
  cost_usd: number | null;
}

export interface DeviceRangeUsage {
  hostname: string;
  updated_at: string;
  is_current: boolean;
  tokens: TokenBreakdown;
  cost_usd: number | null;
}

export interface ProjectUsage {
  project: string;
  tokens: TokenBreakdown;
  cost_usd: number | null;
  claude_tokens: number;
  codex_tokens: number;
}

export interface MonthUsage {
  tokens: number;
  cost_usd: number | null;
  projected_tokens: number;
  projected_cost_usd: number | null;
  days_elapsed: number;
  days_in_month: number;
  budget_tokens: number | null;
}

export interface DashboardData {
  range: string;
  rows: DashboardRow[];
  timezone_note: string;
  devices: DeviceRangeUsage[];
  projects: ProjectUsage[];
  month: MonthUsage;
}

export interface HeatmapCell {
  /** 0 = 월 … 6 = 일 (로컬 tz). */
  weekday: number;
  hour: number;
  total: number;
}

export type Range = "daily30" | "weekly12" | "monthly6";

export const getSummary = () => invoke<Summary>("get_summary");
export const getDevices = () => invoke<DevicesData>("get_devices");

export interface SettingsData {
  version: string;
  tray_display: string; // "tokens" | "cost" | "icon"
  autostart: boolean;
  sync_dir: string | null;
  alerts_enabled: boolean;
  /** Alert thresholds (percent, ascending) — default [30, 50, 70, 90]. */
  alert_thresholds: number[];
  monthly_budget_tokens: number | null;
  date_format: string; // "auto" | "iso" | "us" | "eu"
  percent_display: string; // "used" | "remaining"
}
export const getSettings = () => invoke<SettingsData>("get_settings");
export const setTrayDisplay = (mode: string) =>
  invoke<void>("set_tray_display", { mode });
export const setAutostart = (enabled: boolean) =>
  invoke<void>("set_autostart", { enabled });
export const setAlertsEnabled = (enabled: boolean) =>
  invoke<void>("set_alerts_enabled", { enabled });
export const setAlertThresholds = (thresholds: number[]) =>
  invoke<void>("set_alert_thresholds", { thresholds });
export const setPercentDisplay = (mode: string) =>
  invoke<void>("set_percent_display", { mode });
export const setMonthlyBudget = (tokens: number) =>
  invoke<void>("set_monthly_budget", { tokens });
export const setDateFormat = (format: string) =>
  invoke<void>("set_date_format", { format });
export const pickSyncFolder = () => invoke<string | null>("pick_sync_folder");
export const clearSyncFolder = () => invoke<void>("clear_sync_folder");
export const checkForUpdates = () => invoke<void>("check_for_updates");
export const openLogDir = () => invoke<void>("open_log_dir");
export const openSettings = () => invoke<void>("open_settings");
// scope: "all" | "local" | a device_id (a specific host).
export const getDashboard = (range: Range, scope = "local") =>
  invoke<DashboardData>("get_dashboard", { range, scope });
export const refreshNow = () => invoke<Summary | null>("refresh_now");
export const getHeatmap = () => invoke<HeatmapCell[]>("get_heatmap");
export const exportData = (range: Range, format: "csv" | "json") =>
  invoke<string>("export_data", { range, format });
export const openDashboard = () => invoke<void>("open_dashboard");

export const onUsageUpdated = (
  handler: (summary: Summary) => void,
): Promise<UnlistenFn> =>
  listen<Summary>("usage-updated", (event) => handler(event.payload));

/** Available-update version from the background scan (null = up to date). */
export const getUpdateStatus = () =>
  invoke<string | null>("get_update_status");

// ---- Org reporting (optional; personal use unaffected) ----
export interface OrgStatus {
  url: string | null;
  /** True when url/token come from an IT-managed file (read-only in UI). */
  managed: boolean;
  user_id: string | null;
  registered: boolean;
  last_report: string | null;
  /** Reporting cadence in seconds (fixed). */
  interval_secs: number;
  hostname: string;
}
export const getOrgStatus = () => invoke<OrgStatus>("get_org_status");
/** Send a usage report immediately; resolves to the number of rows sent. */
export const orgReportNow = () => invoke<number>("org_report_now");
export const setOrgConfig = (
  url: string | null,
  token: string | null,
  userId: string | null,
) => invoke<void>("set_org_config", { url, token, userId });
export const orgRegister = () => invoke<void>("org_register");
export const orgDisable = () => invoke<void>("org_disable");
export const onUpdateAvailable = (
  handler: (version: string) => void,
): Promise<UnlistenFn> =>
  listen<string>("update-available", (event) => handler(event.payload));
