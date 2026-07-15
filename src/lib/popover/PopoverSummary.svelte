<script lang="ts">
  import { onMount } from "svelte";
  import {
    getSummary,
    getDevices,
    refreshNow,
    openDashboard,
    onUsageUpdated,
    type Summary,
    type SourceSummary,
    type RateLimitStatus,
    type DevicesData,
    type DeviceSummary,
    type TokenBreakdown,
  } from "../ipc";
  import {
    formatTokens,
    formatCost,
    formatResetTime,
    windowLabel,
    LABEL_ESTIMATED,
    LABEL_MEASURED,
    LABEL_CLI,
    LABEL_COST,
    LABEL_COST_NA,
    LABEL_READ_ERROR,
  } from "../format";
  import { theme } from "../dashboard/charts";
  import Sparkline from "../Sparkline.svelte";

  const t = theme();
  let summary = $state<Summary | null>(null);
  let devices = $state<DevicesData | null>(null);
  // "all" | "__local" (this machine) | a device_id (a specific host).
  let view = $state<string>("__local");
  let refreshing = $state(false);

  function loadDevices() {
    getDevices()
      .then((d) => (devices = d))
      .catch(() => {});
  }

  onMount(() => {
    getSummary().then((s) => (summary = s));
    loadDevices();
    const unlisten = onUsageUpdated((s) => {
      summary = s;
      loadDevices();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  });

  const deviceCount = $derived(devices?.devices.length ?? 1);
  // Show the scope dropdown whenever a sync folder is configured.
  const showToggle = $derived(devices?.sync_enabled ?? false);
  // Other hosts (this machine is the "이 기기" option, not listed twice).
  const otherDevices = $derived(devices?.devices.filter((d) => !d.is_current) ?? []);

  const EMPTY_TK: TokenBreakdown = { input: 0, output: 0, cache_read: 0, cache_creation: 0, total: 0 };

  function combinedTokens(id: string): TokenBreakdown {
    const acc = { input: 0, output: 0, cache_read: 0, cache_creation: 0, total: 0 };
    for (const d of devices?.devices ?? []) {
      const su = d.sources.find((x) => x.id === id);
      if (!su) continue;
      acc.input += su.today_tokens.input;
      acc.output += su.today_tokens.output;
      acc.cache_read += su.today_tokens.cache_read;
      acc.cache_creation += su.today_tokens.cache_creation;
      acc.total += su.today_tokens.total;
    }
    return acc;
  }
  function combinedCost(id: string): number | null {
    let sum: number | null = null;
    for (const d of devices?.devices ?? []) {
      const c = d.sources.find((x) => x.id === id)?.today_cost_usd;
      if (c != null) sum = (sum ?? 0) + c;
    }
    return sum;
  }
  const deviceById = (id: string) => devices?.devices.find((d) => d.device_id === id);
  const deviceTokens = (dev: string, src: string): TokenBreakdown =>
    deviceById(dev)?.sources.find((x) => x.id === src)?.today_tokens ?? EMPTY_TK;
  const deviceCost = (dev: string, src: string): number | null =>
    deviceById(dev)?.sources.find((x) => x.id === src)?.today_cost_usd ?? null;

  // "__local" / sync-off → this machine's live summary; "all" → summed;
  // else the selected host. Only "all" adds the per-device breakdown, and
  // cache-savings/sparkline stay local (this machine) only.
  const combined = $derived(view === "all");
  const isLocalView = $derived(!showToggle || view === "__local");
  const shownTokens = (s: SourceSummary): TokenBreakdown =>
    isLocalView ? s.today_tokens : view === "all" ? combinedTokens(s.id) : deviceTokens(view, s.id);
  const shownCost = (s: SourceSummary): number | null =>
    isLocalView ? s.today_cost_usd : view === "all" ? combinedCost(s.id) : deviceCost(view, s.id);
  const shownSaved = (s: SourceSummary): number | null =>
    isLocalView ? s.today_cache_saved_usd : null;

  const deviceTotal = (d: DeviceSummary): number =>
    d.sources.reduce((n, su) => n + su.today_tokens.total, 0);
  function freshness(iso: string): string {
    const h = Math.floor((Date.now() - new Date(iso).getTime()) / 3_600_000);
    if (h < 1) return "방금";
    if (h < 24) return `${h}시간 전`;
    return `${Math.floor(h / 24)}일 전`;
  }

  // Account strings are "email · plan" (e.g. "…@… · AI CIC Group_2",
  // "…@… · ChatGPT"). Split so the email can truncate and the plan render as
  // a distinct pill instead of one run-on muted line.
  function splitAccount(a: string): { email: string; plan: string | null } {
    const i = a.indexOf(" · ");
    return i === -1
      ? { email: a, plan: null }
      : { email: a.slice(0, i), plan: a.slice(i + 3) };
  }

  async function doRefresh() {
    refreshing = true;
    try {
      const s = await refreshNow();
      if (s) summary = s;
    } finally {
      refreshing = false;
    }
  }

  function healthError(s: SourceSummary): string | null {
    if (typeof s.health === "object" && "error" in s.health) {
      return s.health.error.reason;
    }
    return null;
  }

  function healthNote(s: SourceSummary): string | null {
    if (typeof s.health === "object" && "partial" in s.health) {
      return `${s.health.partial.skipped_lines}줄 건너뜀`;
    }
    return null;
  }

  type UsageRow = { label: string; percent: number; reset: string | null };
  /** Normalize both the real `/usage` (cli) and Codex log (measured) readouts
   *  into one shape so both render as identical session/weekly bar rows. */
  function usageView(
    rl: RateLimitStatus,
  ): { badge: string; rows: UsageRow[] } | null {
    if (rl === "unavailable" || "estimated" in rl) return null;
    if ("cli" in rl) {
      const rows: UsageRow[] = [];
      if (rl.cli.session_percent !== null) {
        rows.push({ label: "세션", percent: rl.cli.session_percent, reset: null });
      }
      for (const w of rl.cli.windows) {
        rows.push({
          label: w.label === "all models" ? "주간" : `주간·${w.label}`,
          percent: w.used_percent,
          reset: w.resets_label,
        });
      }
      return { badge: LABEL_CLI, rows };
    }
    const m = rl.measured;
    // Codex labels the window by its length, not by primary/secondary
    // position — the single window it currently reports is the weekly one.
    const rows: UsageRow[] = [
      {
        label: windowLabel(m.window_minutes),
        percent: m.primary_used_percent,
        reset: formatResetTime(m.resets_at),
      },
    ];
    if (m.secondary_used_percent !== null) {
      rows.push({
        label: "주간",
        percent: m.secondary_used_percent,
        reset: m.secondary_resets_at ? formatResetTime(m.secondary_resets_at) : null,
      });
    }
    return { badge: LABEL_MEASURED, rows };
  }
</script>

<div class="popover">
  <header>
    <span class="app-name">meterly</span>
    <div class="head-right">
      {#if showToggle}
        <select class="scope" bind:value={view} aria-label="기기 선택">
          <option value="all">전체 {deviceCount}대</option>
          <option value="__local">이 기기</option>
          {#each otherDevices as d (d.device_id)}
            <option value={d.device_id}>{d.hostname}</option>
          {/each}
        </select>
      {/if}
      <button class="ghost" onclick={doRefresh} disabled={refreshing}>
        {refreshing ? "…" : "↻"}
      </button>
    </div>
  </header>

  <div class="body">
  {#if summary === null}
    <p class="muted center">불러오는 중…</p>
  {:else}
    {#each summary.sources as s (s.id)}
      <section class="source" style={`--accent:${t.sources[s.id] ?? "#8a8983"}`}>
        <div class="head">
          <span class="name"><span class="dot"></span>{s.display_name}</span>
          <div class="headline">
            {#if isLocalView && !healthError(s)}
              <Sparkline
                values={s.last7_totals}
                color={t.sources[s.id] ?? "#8a8983"}
                width={64}
                height={20}
              />
            {/if}
            {#if healthError(s)}
              <span class="warn" title={healthError(s)}>{LABEL_READ_ERROR}</span>
            {:else}
              <span class="tokens"
                >{formatTokens(shownTokens(s).total)}<span class="unit"> tok</span></span
              >
            {/if}
          </div>
        </div>
        {#if s.account}
          {@const acct = splitAccount(s.account)}
          <div class="acct" title={s.account}>
            <span class="email">{acct.email}</span>
            {#if acct.plan}<span class="plan">{acct.plan}</span>{/if}
          </div>
        {/if}

        {#if !healthError(s)}
          {@const tk = shownTokens(s)}
          {@const cost = shownCost(s)}
          {@const saved = shownSaved(s)}
          <div class="stats">
            <span class="muted io">
              in {formatTokens(tk.input)} · out
              {formatTokens(tk.output)} · cache
              {formatTokens(tk.cache_read + tk.cache_creation)}
            </span>
            <span class="cost" title="구독 요금이 아닌 API 정가 환산값">
              <span class="cost-main"
                >{LABEL_COST} {cost === null ? LABEL_COST_NA : formatCost(cost)}</span
              >
              {#if saved !== null && saved >= 0.01}
                <span class="saved" title="캐시 읽기를 정가 입력으로 환산했을 때 대비 절약액">
                  캐시로 {formatCost(saved)} 절약
                </span>
              {/if}
            </span>
          </div>

          <div class="limits">
            {#if s.rate_limit === "unavailable"}
              <span class="muted small">한도 정보 없음</span>
            {:else if "estimated" in s.rate_limit}
              <span class="muted small">
                <b class="badge">{LABEL_ESTIMATED}</b>
                {s.rate_limit.estimated.window_hours}시간 창
                {formatTokens(s.rate_limit.estimated.window_tokens)} tok ·
                리셋 {formatResetTime(s.rate_limit.estimated.resets_at)}
              </span>
            {:else}
              {@const uv = usageView(s.rate_limit)}
              {#if uv}
                <div class="usage">
                  <span class="lim-head"><b class="badge">{uv.badge}</b></span>
                  {#each uv.rows as r}
                    <div class="uwin" class:warn={r.percent >= 70} class:crit={r.percent >= 90}>
                      <span class="uwin-label" title={r.label}>{r.label}</span>
                      <span class="meter">
                        <span class="fill" style={`width:${Math.min(100, r.percent)}%`}></span>
                      </span>
                      <span class="uwin-pct">{r.percent.toFixed(0)}%</span>
                      {#if r.reset}
                        <span class="muted small reset">리셋 {r.reset}</span>
                      {/if}
                    </div>
                  {/each}
                </div>
              {/if}
            {/if}
          </div>
          {#if healthNote(s)}
            <div class="row"><span class="muted small">{healthNote(s)}</span></div>
          {/if}
        {/if}
      </section>
    {/each}

    {#if combined && devices}
      <section class="devices">
        <div class="dev-title muted">기기별 (오늘)</div>
        {#each devices.devices as d (d.device_id)}
          <div class="dev-row">
            <span class="dev-host">{d.hostname}{d.is_current ? " · 이 기기" : ""}</span>
            <span class="dev-tok">{formatTokens(deviceTotal(d))} tok</span>
            <span class="muted small dev-when">
              {d.is_current ? "실시간" : freshness(d.updated_at)}
            </span>
          </div>
        {/each}
      </section>
    {/if}
  {/if}
  </div>

  <footer>
    <button class="primary" onclick={() => openDashboard()}>대시보드 열기</button>
  </footer>
</div>

<style>
  .popover {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.75rem;
    height: 100%;
    box-sizing: border-box;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex: 0 0 auto;
  }
  /* Scrolls when content (many cards / many devices) exceeds the window;
     header and footer stay pinned so "대시보드 열기" is always reachable. */
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  footer {
    flex: 0 0 auto;
  }
  .app-name {
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .head-right {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  /* Card identity zone: name (left) + sparkline & big token figure (right),
     then the account on its own full-width line so the email isn't boxed
     into the left half and truncated. */
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 10px;
  }
  .headline {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
    flex: 0 0 auto;
  }
  .acct {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    width: 100%;
    margin-top: -2px;
  }
  .email {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    opacity: 0.6;
    font-size: 0.76rem;
  }
  .plan {
    flex: 0 0 auto;
    font-size: 0.66rem;
    font-weight: 600;
    padding: 1px 7px;
    border-radius: 999px;
    background: rgba(128, 128, 128, 0.16);
    white-space: nowrap;
    opacity: 0.9;
  }
  .scope {
    font: inherit;
    font-size: 11.5px;
    padding: 2px 6px;
    border-radius: 7px;
    border: 1px solid rgba(128, 128, 128, 0.35);
    background: rgba(128, 128, 128, 0.12);
    color: inherit;
    max-width: 150px;
    cursor: pointer;
  }
  .devices {
    border: 1px solid var(--border, rgba(128, 128, 128, 0.25));
    border-radius: 10px;
    padding: 0.5rem 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .dev-title {
    font-size: 11.5px;
    font-weight: 600;
  }
  .dev-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .dev-host {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dev-tok {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .dev-when {
    flex: 0 0 auto;
  }
  .source {
    border: 1px solid var(--border, rgba(128, 128, 128, 0.25));
    border-radius: 10px;
    padding: 0.6rem 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
  }
  .name {
    font-weight: 600;
    display: inline-flex;
    align-items: center;
    gap: 0.4em;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 3px;
    background: var(--accent, #8a8983);
    display: inline-block;
  }
  .tokens {
    font-variant-numeric: tabular-nums;
    font-weight: 700;
    font-size: 1.15rem;
    line-height: 1;
    white-space: nowrap;
  }
  .unit {
    font-size: 0.72rem;
    font-weight: 600;
    opacity: 0.6;
  }
  /* Economics zone — usage detail (left) + cost/savings (right), divided
     from the identity zone above. */
  .stats {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 8px;
    padding-top: 0.5rem;
    border-top: 1px solid rgba(128, 128, 128, 0.16);
  }
  .io {
    font-size: 0.76rem;
  }
  /* Limits zone — badge + usage bars, divided from economics. */
  .limits {
    padding-top: 0.5rem;
    border-top: 1px solid rgba(128, 128, 128, 0.16);
  }
  .lim-head {
    display: block;
    margin-bottom: 6px;
  }
  .warn {
    color: #c47912;
    font-weight: 600;
    font-size: 0.85rem;
  }
  .muted {
    opacity: 0.65;
    font-size: 0.8rem;
  }
  .cost {
    display: inline-flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
    font-size: 0.76rem;
    opacity: 0.9;
    flex: 0 0 auto;
    text-align: right;
  }
  .cost-main {
    white-space: nowrap;
  }
  .saved {
    color: #008300;
    font-size: 0.72rem;
    font-weight: 600;
  }
  .small {
    font-size: 0.72rem;
  }
  .center {
    text-align: center;
  }
  .badge {
    border: 1px solid currentColor;
    border-radius: 4px;
    padding: 0 0.25em;
    font-size: 0.7rem;
    font-weight: 600;
  }
  .meter {
    flex: 0 0 56px;
    height: 6px;
    border-radius: 3px;
    background: rgba(128, 128, 128, 0.25);
    overflow: hidden;
  }
  .meter .fill {
    display: block;
    height: 100%;
    background: #4f8ef7;
  }
  .usage {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 100%;
    margin-top: 2px;
  }
  .uwin {
    display: flex;
    align-items: center;
    gap: 8px;
    row-gap: 3px;
    flex-wrap: wrap;
  }
  .uwin .meter {
    flex: 1 1 60px;
  }
  .uwin-label {
    flex: 0 0 auto;
    min-width: 3.2em;
    font-size: 0.78rem;
  }
  .uwin-pct {
    flex: 0 0 auto;
    font-variant-numeric: tabular-nums;
    font-size: 0.78rem;
  }
  .uwin .reset {
    flex-basis: 100%;
  }
  /* Usage-rate emphasis: amber ≥70%, red ≥90%. */
  .uwin.warn .fill {
    background: #e0a83a;
  }
  .uwin.crit .fill {
    background: #e0524f;
  }
  .uwin.crit .uwin-pct {
    color: #e0524f;
    font-weight: 700;
  }
  footer {
    margin-top: auto;
    display: flex;
  }
  button {
    font: inherit;
    border-radius: 8px;
    border: 1px solid rgba(128, 128, 128, 0.35);
    background: transparent;
    color: inherit;
    cursor: pointer;
    padding: 0.4rem 0.7rem;
  }
  button.primary {
    flex: 1;
    background: #4f8ef7;
    border-color: #4f8ef7;
    color: white;
    font-weight: 600;
  }
  button.ghost {
    border: none;
    font-size: 1rem;
    padding: 0.1rem 0.4rem;
  }
  button:disabled {
    opacity: 0.5;
  }
</style>
