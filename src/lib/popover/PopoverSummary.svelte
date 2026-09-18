<script lang="ts">
  import { onMount } from "svelte";
  import {
    getSummary,
    getDevices,
    getSettings,
    getUpdateStatus,
    onUpdateAvailable,
    checkForUpdates,
    refreshNow,
    openDashboard,
    openSettings,
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
    formatResetLabel,
    parseResetDate,
    formatCountdown,
    windowLabel,
    type DateFormat,
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
  // Display preferences (Settings). Re-read on each refresh so a change in
  // the Settings window applies without an app restart.
  let dateFmt = $state<DateFormat>("auto");
  // "used" (사용한 양) | "remaining" (남은 양) for the limit gauges.
  let pctMode = $state<string>("used");
  // Available-update version from the background scan (null = up to date).
  let updateVersion = $state<string | null>(null);
  // Ticks every 30s so reset countdowns stay live while the popover is open.
  let now = $state<number>(Date.now());

  function loadDevices() {
    getDevices()
      .then((d) => (devices = d))
      .catch(() => {});
  }
  function loadDateFmt() {
    getSettings()
      .then((s) => {
        dateFmt = (s.date_format as DateFormat) ?? "auto";
        pctMode = s.percent_display ?? "used";
      })
      .catch(() => {});
  }

  onMount(() => {
    getSummary().then((s) => (summary = s));
    loadDevices();
    loadDateFmt();
    getUpdateStatus()
      .then((v) => (updateVersion = v))
      .catch(() => {});
    const unlisten = onUsageUpdated((s) => {
      summary = s;
      loadDevices();
      loadDateFmt();
    });
    const unlistenUpdate = onUpdateAvailable((v) => (updateVersion = v));
    const tick = setInterval(() => (now = Date.now()), 30_000);
    return () => {
      unlisten.then((fn) => fn());
      unlistenUpdate.then((fn) => fn());
      clearInterval(tick);
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
  function combinedSaved(id: string): number | null {
    let sum: number | null = null;
    for (const d of devices?.devices ?? []) {
      const sv = d.sources.find((x) => x.id === id)?.today_cache_saved_usd;
      if (sv != null) sum = (sum ?? 0) + sv;
    }
    return sum;
  }
  // Element-wise sum of every device's 7-day series for a source.
  function combinedLast7(id: string): number[] {
    const acc = [0, 0, 0, 0, 0, 0, 0];
    for (const d of devices?.devices ?? []) {
      const l7 = d.sources.find((x) => x.id === id)?.last7_totals ?? [];
      l7.forEach((v, i) => (acc[i] += v));
    }
    return acc;
  }
  const deviceById = (id: string) => devices?.devices.find((d) => d.device_id === id);
  const deviceSource = (dev: string, src: string) =>
    deviceById(dev)?.sources.find((x) => x.id === src);
  const deviceTokens = (dev: string, src: string): TokenBreakdown =>
    deviceSource(dev, src)?.today_tokens ?? EMPTY_TK;
  const deviceCost = (dev: string, src: string): number | null =>
    deviceSource(dev, src)?.today_cost_usd ?? null;
  const deviceSaved = (dev: string, src: string): number | null =>
    deviceSource(dev, src)?.today_cache_saved_usd ?? null;
  const deviceLast7 = (dev: string, src: string): number[] =>
    deviceSource(dev, src)?.last7_totals ?? [];

  // "__local" / sync-off → this machine's live summary; "all" → summed across
  // devices; else the selected host. Tokens, cost, cache-savings AND the
  // sparkline are all resolved per-scope so every view renders identically.
  const combined = $derived(view === "all");
  const isLocalView = $derived(!showToggle || view === "__local");
  const shownTokens = (s: SourceSummary): TokenBreakdown =>
    isLocalView ? s.today_tokens : view === "all" ? combinedTokens(s.id) : deviceTokens(view, s.id);
  const shownCost = (s: SourceSummary): number | null =>
    isLocalView ? s.today_cost_usd : view === "all" ? combinedCost(s.id) : deviceCost(view, s.id);
  const shownSaved = (s: SourceSummary): number | null =>
    isLocalView ? s.today_cache_saved_usd : view === "all" ? combinedSaved(s.id) : deviceSaved(view, s.id);
  const shownLast7 = (s: SourceSummary): number[] =>
    isLocalView ? s.last7_totals : view === "all" ? combinedLast7(s.id) : deviceLast7(view, s.id);

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
  // The re-login command for a signed-out provider.
  function loginCmd(id: SourceSummary["id"]): string {
    return id === "codex" ? "codex login" : "claude 실행 후 /login";
  }

  type UsageRow = {
    label: string;
    percent: number;
    reset: string | null;
    resetAt: Date | null;
  };
  /** Normalize both the real `/usage` (cli) and Codex log (measured) readouts
   *  into one shape so both render as identical session/weekly bar rows. */
  function usageView(
    rl: RateLimitStatus,
  ): { badge: string; rows: UsageRow[] } | null {
    if (rl === "unavailable" || "estimated" in rl) return null;
    if ("cli" in rl) {
      const rows: UsageRow[] = [];
      if (rl.cli.session_percent !== null) {
        // The 5-hour session window carries its own reset — show it counting
        // down like the weekly rows instead of leaving the row bare.
        rows.push({
          label: "세션",
          percent: rl.cli.session_percent,
          reset: formatResetLabel(rl.cli.session_resets_at, dateFmt),
          resetAt: parseResetDate(rl.cli.session_resets_at),
        });
      }
      for (const w of rl.cli.windows) {
        rows.push({
          label: w.label === "all models" ? "주간" : `주간·${w.label}`,
          percent: w.used_percent,
          reset: formatResetLabel(w.resets_label, dateFmt),
          resetAt: parseResetDate(w.resets_label),
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
        reset: formatResetTime(m.resets_at, dateFmt),
        resetAt: parseResetDate(m.resets_at),
      },
    ];
    if (m.secondary_used_percent !== null) {
      rows.push({
        label: "주간",
        percent: m.secondary_used_percent,
        reset: m.secondary_resets_at ? formatResetTime(m.secondary_resets_at, dateFmt) : null,
        resetAt: parseResetDate(m.secondary_resets_at),
      });
    }
    return { badge: LABEL_MEASURED, rows };
  }
</script>

<div class="popover">
  <header>
    <span class="app-name"><span class="orb" aria-hidden="true"></span>meterly</span>
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
      <button class="ghost" onclick={doRefresh} disabled={refreshing} title="새로고침">
        {refreshing ? "…" : "↻"}
      </button>
      <button class="ghost" onclick={() => openSettings()} title="설정" aria-label="설정">
        ⚙
      </button>
    </div>
  </header>

  {#if updateVersion}
    <button
      class="update-banner"
      title="클릭하면 설치 안내가 열립니다"
      onclick={() => checkForUpdates()}
    >
      ↑ 새 버전 v{updateVersion} 업데이트 가능 — 설치하기
    </button>
  {/if}

  <div class="body">
  {#if summary === null}
    <p class="muted center">불러오는 중…</p>
  {:else}
    {#each summary.sources as s (s.id)}
      <section class="source" style={`--accent:${t.sources[s.id] ?? "#8a8983"}`}>
        <div class="head">
          <span class="name"><span class="dot"></span>{s.display_name}</span>
          <div class="headline">
            {#if healthError(s)}
              <span class="warn" title={healthError(s)}>{LABEL_READ_ERROR}</span>
            {:else}
              <span
                class="tokens"
                title="이 기기의 로컬 세션 토큰 합계 · 계정 무관 (로그에 계정 구분이 없어 여러 계정 사용분이 합쳐집니다). 아래 한도 게이지만 현재 로그인 계정 기준입니다."
                >{formatTokens(shownTokens(s).total)}<span class="unit">tok</span></span
              >
              <Sparkline
                values={shownLast7(s)}
                color={t.sources[s.id] ?? "#8a8983"}
                width={120}
                height={26}
              />
            {/if}
          </div>
        </div>
        {#if s.account}
          {@const acct = splitAccount(s.account)}
          <div class="acct" title={`${s.account} · 현재 로그인 계정 (한도 게이지 기준)`}>
            <span class="email">{acct.email}</span>
            {#if acct.plan}<span class="plan">{acct.plan}</span>{/if}
          </div>
        {/if}

        {#if !healthError(s)}
          {@const tk = shownTokens(s)}
          {@const cost = shownCost(s)}
          {@const saved = shownSaved(s)}
          <div class="tiles">
            <div class="tile">
              <span class="tile-k">in</span>
              <span class="tile-v">{formatTokens(tk.input)}</span>
            </div>
            <div class="tile">
              <span class="tile-k">out</span>
              <span class="tile-v">{formatTokens(tk.output)}</span>
            </div>
            <div class="tile">
              <span class="tile-k">cache</span>
              <span class="tile-v">{formatTokens(tk.cache_read + tk.cache_creation)}</span>
            </div>
          </div>
          <div class="cost" title="구독 요금이 아닌 API 정가 환산값">
            <span class="cost-k">{LABEL_COST}</span>
            <span class="cost-v">{cost === null ? LABEL_COST_NA : formatCost(cost)}</span>
            {#if saved !== null && saved >= 0.01}
              <span class="saved" title="캐시 읽기를 정가 입력으로 환산했을 때 대비 절약액">
                캐시 {formatCost(saved)} 절약
              </span>
            {/if}
          </div>

          <div class="limits">
            {#if s.auth === "logged_out"}
              <span class="login-needed">
                <b class="badge warn">로그인 필요</b>
                <span
                  >터미널에서 <code>{loginCmd(s.id)}</code> 후 다시 로그인하세요</span
                >
              </span>
            {:else if s.rate_limit === "unavailable"}
              <span class="muted small">한도 정보 없음</span>
            {:else if "estimated" in s.rate_limit}
              <span class="muted small est">
                <b class="badge">{LABEL_ESTIMATED}</b>
                {s.rate_limit.estimated.window_hours}시간 창
                {formatTokens(s.rate_limit.estimated.window_tokens)} tok ·
                <span title={`리셋 ${formatResetTime(s.rate_limit.estimated.resets_at, dateFmt)}`}>
                  리셋 {formatCountdown(new Date(s.rate_limit.estimated.resets_at), now)}
                </span>
              </span>
            {:else}
              {@const uv = usageView(s.rate_limit)}
              {#if uv}
                <div class="usage">
                  <span class="lim-head"><b class="badge live">{uv.badge}</b></span>
                  {#each uv.rows as r}
                    {@const shown = pctMode === "remaining" ? Math.max(0, 100 - r.percent) : r.percent}
                    <!-- warn/crit always key on USED percent, whatever is shown. -->
                    <div class="uwin" class:warn={r.percent >= 70} class:crit={r.percent >= 90}>
                      <span class="uwin-label" title={r.label}>{r.label}</span>
                      <span class="meter">
                        <span class="fill" style={`width:${Math.min(100, shown)}%`}></span>
                      </span>
                      <span class="uwin-meta">
                        <span class="uwin-pct">
                          {shown.toFixed(0)}%{pctMode === "remaining" ? " 남음" : ""}
                        </span>
                        {#if r.reset}
                          <span class="reset" title={`리셋 ${r.reset}`}>
                            {r.resetAt && r.resetAt.getTime() > now
                              ? `${formatCountdown(r.resetAt, now)} 리셋`
                              : `리셋 ${r.reset}`}
                          </span>
                        {/if}
                      </span>
                    </div>
                  {/each}
                </div>
              {/if}
              {#if s.auth === "stale"}
                <span class="muted small login-hint">
                  로그인이 만료됐을 수 있어요 · 다시 로그인하면 실제 한도가 표시됩니다
                </span>
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
  /* ---------- Shell ---------- */
  .popover {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    padding: 0.85rem 0.85rem 0.8rem;
    height: 100%;
    box-sizing: border-box;
    /* Ambient light at the top so cards float on depth, not on flat Canvas. */
    background:
      radial-gradient(
        120% 70% at 50% -20%,
        color-mix(in srgb, #4f8ef7 14%, transparent),
        transparent 60%
      );
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex: 0 0 auto;
    padding: 0 0.15rem;
  }
  .app-name {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    font-weight: 800;
    font-size: 1.05rem;
    letter-spacing: -0.01em;
  }
  .orb {
    width: 11px;
    height: 11px;
    border-radius: 50%;
    background: linear-gradient(135deg, #eb6834, #4f8ef7);
    box-shadow: 0 0 10px color-mix(in srgb, #4f8ef7 55%, transparent);
  }
  .head-right {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  /* Scrolls when content exceeds the window; header/footer stay pinned. */
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    scrollbar-width: none;
  }
  .body::-webkit-scrollbar {
    display: none;
  }
  footer {
    flex: 0 0 auto;
    margin-top: auto;
    display: flex;
  }

  /* ---------- Update banner ---------- */
  .update-banner {
    flex: 0 0 auto;
    font: inherit;
    font-size: 0.78rem;
    font-weight: 600;
    text-align: center;
    padding: 0.5rem 0.7rem;
    border-radius: 12px;
    border: 1px solid color-mix(in srgb, #4f8ef7 40%, transparent);
    background: color-mix(in srgb, #4f8ef7 14%, transparent);
    color: inherit;
    cursor: pointer;
  }
  .update-banner:hover {
    background: color-mix(in srgb, #4f8ef7 24%, transparent);
  }

  /* ---------- Provider card ---------- */
  .source {
    position: relative;
    border-radius: 18px;
    padding: 0.85rem 0.95rem 0.9rem;
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    /* Glass: a soft wash of the provider colour bleeding from the top-left,
       over a faint neutral fill; hairline border tinted the same. */
    background:
      radial-gradient(
        90% 60% at 0% 0%,
        color-mix(in srgb, var(--accent, #8a8983) 16%, transparent),
        transparent 70%
      ),
      color-mix(in srgb, currentColor 4.5%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent, #8a8983) 26%, transparent);
    box-shadow:
      inset 0 1px 0 color-mix(in srgb, #fff 7%, transparent),
      0 10px 30px -18px color-mix(in srgb, var(--accent, #8a8983) 45%, transparent);
  }

  /* Identity zone: name (left) · hero number + trend band (right). */
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 10px;
  }
  .name {
    font-weight: 700;
    font-size: 1rem;
    letter-spacing: -0.01em;
    display: inline-flex;
    align-items: center;
    gap: 0.5em;
    padding-top: 0.15rem;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--accent, #8a8983);
    display: inline-block;
    box-shadow:
      0 0 0 3px color-mix(in srgb, var(--accent, #8a8983) 20%, transparent),
      0 0 12px color-mix(in srgb, var(--accent, #8a8983) 60%, transparent);
  }
  .headline {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
    flex: 0 0 auto;
  }
  .tokens {
    font-variant-numeric: tabular-nums;
    font-weight: 800;
    font-size: 1.75rem;
    letter-spacing: -0.03em;
    line-height: 1;
    white-space: nowrap;
    display: inline-flex;
    align-items: baseline;
    gap: 0.25em;
  }
  .unit {
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    opacity: 0.5;
  }
  .acct {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    width: 100%;
    margin-top: -0.35rem;
  }
  .email {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    opacity: 0.55;
    font-size: 0.74rem;
  }
  .plan {
    flex: 0 0 auto;
    font-size: 0.64rem;
    font-weight: 700;
    letter-spacing: 0.02em;
    padding: 1px 8px;
    border-radius: 999px;
    color: var(--accent, #8a8983);
    background: color-mix(in srgb, var(--accent, #8a8983) 14%, transparent);
    white-space: nowrap;
  }

  /* Economics: three stat tiles, then the cost line. */
  .tiles {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 6px;
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 0.42rem 0.6rem;
    border-radius: 11px;
    background: color-mix(in srgb, currentColor 6%, transparent);
    min-width: 0;
  }
  .tile-k {
    font-size: 0.6rem;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    opacity: 0.5;
  }
  .tile-v {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    font-size: 0.86rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cost {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.76rem;
    padding: 0 0.15rem;
  }
  .cost-k {
    opacity: 0.55;
  }
  .cost-v {
    font-variant-numeric: tabular-nums;
    font-weight: 700;
    margin-right: auto;
  }
  .saved {
    font-size: 0.68rem;
    font-weight: 700;
    color: #22a559;
    padding: 1px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, #22a559 14%, transparent);
    white-space: nowrap;
  }

  /* Limits: pill badge + glowing gauges. */
  .limits {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .lim-head {
    display: block;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 0.64rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    padding: 2px 9px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 9%, transparent);
    opacity: 0.85;
  }
  .badge.live {
    color: #22a559;
    background: color-mix(in srgb, #22a559 14%, transparent);
    opacity: 1;
  }
  .badge.live::before {
    content: "";
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    box-shadow: 0 0 6px currentColor;
    animation: pulse 2.2s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.35; }
  }
  @media (prefers-reduced-motion: reduce) {
    .badge.live::before { animation: none; }
  }
  .badge.warn {
    color: #d98324;
    background: color-mix(in srgb, #d98324 14%, transparent);
    opacity: 1;
  }
  .est,
  .login-hint {
    display: inline-flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    line-height: 1.5;
  }
  .usage {
    display: flex;
    flex-direction: column;
    gap: 9px;
    width: 100%;
    margin-top: 2px;
  }
  .uwin {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    row-gap: 2px;
  }
  .uwin-label {
    flex: 0 0 auto;
    min-width: 3.4em;
    font-size: 0.76rem;
    font-weight: 600;
    opacity: 0.8;
  }
  .meter {
    flex: 1 1 60px;
    height: 8px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 10%, transparent);
    overflow: hidden;
  }
  .meter .fill {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: linear-gradient(
      90deg,
      var(--accent, #4f8ef7),
      color-mix(in srgb, var(--accent, #4f8ef7) 55%, #ffffff)
    );
    box-shadow: 0 0 10px color-mix(in srgb, var(--accent, #4f8ef7) 55%, transparent);
    transition: width 0.4s ease;
  }
  .uwin-meta {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: baseline;
    gap: 7px;
    white-space: nowrap;
  }
  .uwin-pct {
    font-variant-numeric: tabular-nums;
    font-size: 0.8rem;
    font-weight: 700;
  }
  .reset {
    font-size: 0.68rem;
    opacity: 0.5;
  }
  /* Usage emphasis: amber ≥70%, red ≥90% — the gauge and the number agree. */
  .uwin.warn .fill {
    background: linear-gradient(90deg, #e0a83a, #f2c866);
    box-shadow: 0 0 10px color-mix(in srgb, #e0a83a 55%, transparent);
  }
  .uwin.crit .fill {
    background: linear-gradient(90deg, #e0524f, #ff8a7a);
    box-shadow: 0 0 10px color-mix(in srgb, #e0524f 60%, transparent);
  }
  .uwin.warn .uwin-pct { color: #e0a83a; }
  .uwin.crit .uwin-pct { color: #e0524f; }

  .login-needed {
    display: flex;
    align-items: center;
    gap: 0.5em;
    font-size: 0.78rem;
    flex-wrap: wrap;
  }
  .login-needed code {
    font-size: 0.72rem;
    padding: 0.1em 0.45em;
    border-radius: 6px;
    background: color-mix(in srgb, currentColor 10%, transparent);
  }

  /* ---------- Devices (전체 view) ---------- */
  .devices {
    border-radius: 14px;
    padding: 0.6rem 0.8rem;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: color-mix(in srgb, currentColor 4.5%, transparent);
    border: 1px solid color-mix(in srgb, currentColor 10%, transparent);
  }
  .dev-title {
    font-size: 0.62rem;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
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
    font-size: 0.82rem;
  }
  .dev-tok {
    font-variant-numeric: tabular-nums;
    font-weight: 700;
    font-size: 0.82rem;
  }
  .dev-when {
    flex: 0 0 auto;
  }

  /* ---------- Misc ---------- */
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
  }
  .warn {
    color: #c47912;
    font-weight: 600;
    font-size: 0.85rem;
  }
  .muted {
    opacity: 0.6;
    font-size: 0.8rem;
  }
  .small {
    font-size: 0.72rem;
  }
  .center {
    text-align: center;
  }
  .scope {
    font: inherit;
    font-size: 11.5px;
    padding: 3px 8px;
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, currentColor 18%, transparent);
    background: color-mix(in srgb, currentColor 6%, transparent);
    color: inherit;
    max-width: 150px;
    cursor: pointer;
    margin-right: 4px;
  }

  /* ---------- Buttons ---------- */
  button {
    font: inherit;
    border-radius: 10px;
    border: 1px solid color-mix(in srgb, currentColor 18%, transparent);
    background: transparent;
    color: inherit;
    cursor: pointer;
    padding: 0.4rem 0.7rem;
  }
  button.primary {
    flex: 1;
    border: none;
    border-radius: 13px;
    padding: 0.62rem 0.8rem;
    font-weight: 700;
    letter-spacing: -0.01em;
    color: #fff;
    background: linear-gradient(135deg, #4f8ef7, #6a5cf5);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.22),
      0 10px 22px -10px rgba(79, 142, 247, 0.75);
    transition: transform 0.12s ease, filter 0.12s ease;
  }
  button.primary:hover {
    filter: brightness(1.06);
  }
  button.primary:active {
    transform: translateY(1px);
  }
  button.ghost {
    border: none;
    width: 30px;
    height: 30px;
    padding: 0;
    border-radius: 50%;
    font-size: 1rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    opacity: 0.75;
  }
  button.ghost:hover {
    opacity: 1;
    background: color-mix(in srgb, currentColor 9%, transparent);
  }
  button:disabled {
    opacity: 0.45;
  }
</style>
