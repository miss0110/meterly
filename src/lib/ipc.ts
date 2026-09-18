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
        /** When the 5-hour session window resets (ISO, or legacy English text). */
        session_resets_at: string | null;
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
  /** Whether the source appears signed in — drives the re-login prompt.
   *  "stale" = can't confirm and the plan data expired (usually a lapsed login). */
  auth: "ok" | "logged_out" | "stale";
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

/** This week vs last week by weekday (0 = 월 … 6 = 일), scope-aware. */
export interface WeekCompare {
  /** Monday of this week, YYYY-MM-DD (local). */
  week_start: string;
  today_weekday: number;
  this_week: number[];
  last_week: number[];
  this_cost: number | null;
  last_cost: number | null;
  /** Monday through today's weekday, both weeks (like-for-like). */
  this_to_date: number;
  last_to_date: number;
}

export interface DashboardData {
  range: string;
  rows: DashboardRow[];
  timezone_note: string;
  devices: DeviceRangeUsage[];
  projects: ProjectUsage[];
  month: MonthUsage;
  weeks: WeekCompare;
}

/** One session's usage for a day — "가장 비싼 세션" list (this device only). */
export interface SessionUsage {
  session_id: string;
  source: SourceId;
  project: string | null;
  /** Model with the most tokens in the session. */
  model: string | null;
  /** Distinct models used (>1 = main + subagent slices). */
  model_count: number;
  tokens: TokenBreakdown;
  cost_usd: number | null;
  first_at: string;
  last_at: string;
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
  /** Model-price auto refresh from the public feed (default on). */
  price_sync_enabled: boolean;
  /** When prices were last fetched (ISO), or null if never. */
  price_sync_fetched_at: string | null;
  /** Active fetched model prices (0 = built-in table only). */
  price_sync_models: number;
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
export const setPriceSync = (enabled: boolean) =>
  invoke<void>("set_price_sync", { enabled });
/** Refresh model prices now; resolves to how many prices were installed. */
export const refreshPrices = () => invoke<number>("refresh_prices");
export const checkForUpdates = () => invoke<void>("check_for_updates");
export const openLogDir = () => invoke<void>("open_log_dir");
export const openSettings = () => invoke<void>("open_settings");
// scope: "all" | "local" | a device_id (a specific host).
export const getDashboard = (range: Range, scope = "local") =>
  invoke<DashboardData>("get_dashboard", { range, scope });
export const refreshNow = () => invoke<Summary | null>("refresh_now");
/** source: "all" (both tools) | a SourceId to narrow the heatmap to one tool. */
export const getHeatmap = (source: SourceId | "all" = "all") =>
  invoke<HeatmapCell[]>("get_heatmap", { source: source === "all" ? null : source });
/** day: YYYY-MM-DD local (default today); highest cost first. */
export const getSessions = (day?: string, limit = 5) =>
  invoke<SessionUsage[]>("get_sessions", { day: day ?? null, limit });
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
  /** Sources included in reports ("claude_code" | "codex"). */
  sources: string[];
  /** Last actionable server rejection (e.g. unknown identifier), else null. */
  last_error: string | null;
  /** Which devices to report: "this" | "all" | "selected". */
  scope: "this" | "all" | "selected";
  /** Device ids included when scope === "selected". */
  scope_devices: string[];
}
export const getOrgStatus = () => invoke<OrgStatus>("get_org_status");
export const setOrgSources = (sources: string[]) =>
  invoke<void>("set_org_sources", { sources });
/** Set reporting scope: "this" | "all" | "selected" (devices apply to "selected"). */
export const setOrgScope = (scope: string, devices: string[]) =>
  invoke<void>("set_org_scope", { scope, devices });
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
