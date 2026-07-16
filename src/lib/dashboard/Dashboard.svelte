<script lang="ts">
  import { onMount } from "svelte";
  import {
    getDashboard,
    getSummary,
    getDevices,
    getHeatmap,
    exportData,
    onUsageUpdated,
    type DashboardData,
    type HeatmapCell,
    type Range,
    type Summary,
  } from "../ipc";
  import {
    formatTokens,
    formatCost,
    LABEL_COST,
    LABEL_COST_NA,
    LABEL_ESTIMATED,
    LABEL_MEASURED,
  } from "../format";
  import {
    renderTrendChart,
    renderCompositionChart,
    renderCostChart,
    renderModelChart,
    theme,
    SOURCE_LABELS,
  } from "./charts";
  import Sparkline from "../Sparkline.svelte";

  const RANGES: { key: Range; label: string }[] = [
    { key: "daily30", label: "일 (30일)" },
    { key: "weekly12", label: "주 (12주)" },
    { key: "monthly6", label: "월 (6개월)" },
  ];

  let range = $state<Range>("daily30");
  // "all" | "local" (this machine) | a device_id (a specific host).
  let scope = $state<string>("local");
  let syncEnabled = $state(false);
  let otherHosts = $state<{ id: string; hostname: string }[]>([]);
  let data = $state<DashboardData | null>(null);
  let summary = $state<Summary | null>(null);
  let heatmap = $state<HeatmapCell[]>([]);
  let exportMsg = $state<string | null>(null);
  let trendCanvas: HTMLCanvasElement;
  let compositionCanvas: HTMLCanvasElement;
  let costCanvas: HTMLCanvasElement;
  let modelCanvas: HTMLCanvasElement;

  const t = theme();

  async function load() {
    data = await getDashboard(range, scope);
    renderTrendChart(trendCanvas, data);
    renderCompositionChart(compositionCanvas, data);
    renderCostChart(costCanvas, data);
    renderModelChart(modelCanvas, data);
    heatmap = await getHeatmap();
  }

  function selectScope(s: string) {
    scope = s;
    load();
  }
  function freshness(iso: string): string {
    const h = Math.floor((Date.now() - new Date(iso).getTime()) / 3_600_000);
    if (h < 1) return "방금";
    if (h < 24) return `${h}시간 전`;
    return `${Math.floor(h / 24)}일 전`;
  }

  const WEEKDAYS = ["월", "화", "수", "목", "금", "토", "일"];
  const heatmapMax = $derived(Math.max(...heatmap.map((c) => c.total), 1));

  function heatColor(total: number): string {
    // Faint base (not transparent) so the gapless grid still shows empty cells.
    if (total === 0) return "color-mix(in srgb, currentColor 8%, transparent)";
    // sqrt scale: heavy-tail token counts would otherwise flatten the ramp.
    const idx = Math.min(
      t.seq.length - 1,
      1 + Math.floor(Math.sqrt(total / heatmapMax) * (t.seq.length - 1)),
    );
    return t.seq[idx];
  }

  function cellAt(wd: number, hour: number): number {
    return heatmap.find((c) => c.weekday === wd && c.hour === hour)?.total ?? 0;
  }

  async function doExport(format: "csv" | "json") {
    try {
      const path = await exportData(range, format);
      exportMsg = `저장됨: ${path}`;
    } catch (e) {
      exportMsg = `내보내기 실패: ${e}`;
    }
    setTimeout(() => (exportMsg = null), 6000);
  }

  onMount(() => {
    getSummary().then((s) => (summary = s));
    getDevices()
      .then((d) => {
        syncEnabled = d.sync_enabled;
        otherHosts = d.devices
          .filter((x) => !x.is_current)
          .map((x) => ({ id: x.device_id, hostname: x.hostname }));
      })
      .catch(() => {});
    load();
    const unlisten = onUsageUpdated((s) => {
      summary = s;
      load();
    });
    // Re-render charts when the OS theme flips (colors are theme-selected).
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onTheme = () => load();
    mq.addEventListener("change", onTheme);
    return () => {
      unlisten.then((fn) => fn());
      mq.removeEventListener("change", onTheme);
    };
  });

  function selectRange(r: Range) {
    range = r;
    load();
  }

  /** 전일 대비 변화 (last7: oldest→today — [5]=어제, [6]=오늘). */
  function delta(last7: number[]): { text: string; dir: "up" | "down" | "flat" } {
    const today = last7[6] ?? 0;
    const yesterday = last7[5] ?? 0;
    if (yesterday === 0) return { text: today > 0 ? "새 사용" : "—", dir: "flat" };
    const pct = ((today - yesterday) / yesterday) * 100;
    if (Math.abs(pct) < 1) return { text: "≈ 어제", dir: "flat" };
    return {
      text: `${pct > 0 ? "▲" : "▼"} ${Math.abs(pct).toFixed(0)}% vs 어제`,
      dir: pct > 0 ? "up" : "down",
    };
  }
</script>

<div class="dashboard">
  <header>
    <h1>meterly</h1>
    <nav>
      {#if syncEnabled}
        <select
          class="scope-select"
          value={scope}
          onchange={(e) => selectScope((e.currentTarget as HTMLSelectElement).value)}
        >
          <option value="all">전체</option>
          <option value="local">이 기기</option>
          {#each otherHosts as h (h.id)}
            <option value={h.id}>{h.hostname}</option>
          {/each}
        </select>
        <span class="nav-sep"></span>
      {/if}
      {#each RANGES as r (r.key)}
        <button class:active={range === r.key} onclick={() => selectRange(r.key)}>
          {r.label}
        </button>
      {/each}
      <span class="nav-sep"></span>
      <button title="현재 범위 집계를 CSV로 저장" onclick={() => doExport("csv")}>CSV</button>
      <button title="현재 범위 집계를 JSON으로 저장" onclick={() => doExport("json")}>JSON</button>
    </nav>
  </header>
  {#if exportMsg}
    <div class="toast">{exportMsg}</div>
  {/if}

  {#if summary}
    <section class="cards">
      {#each summary.sources as s (s.id)}
        <div class="card" style={`--accent:${t.sources[s.id] ?? "#8a8983"}`}>
          <div class="card-head">
            <span class="card-title">
              <span class="dot"></span>
              {SOURCE_LABELS[s.id] ?? s.display_name} · 오늘
            </span>
            <span class="delta {delta(s.last7_totals).dir}">
              {delta(s.last7_totals).text}
            </span>
          </div>
          <div class="card-row">
            <span class="card-big">{formatTokens(s.today_tokens.total)}</span>
            <Sparkline values={s.last7_totals} color={t.sources[s.id] ?? "#8a8983"} />
          </div>
          <div class="card-sub">
            {LABEL_COST}
            {s.today_cost_usd === null ? LABEL_COST_NA : formatCost(s.today_cost_usd)}
            {#if s.today_cache_saved_usd !== null && s.today_cache_saved_usd >= 0.01}
              · <span class="saved">캐시로 {formatCost(s.today_cache_saved_usd)} 절약</span>
            {/if}
            {#if s.rate_limit !== "unavailable" && "estimated" in s.rate_limit}
              · <b>{LABEL_ESTIMATED}</b> 창 {formatTokens(s.rate_limit.estimated.window_tokens)}
            {:else if s.rate_limit !== "unavailable" && "measured" in s.rate_limit}
              · <b>{LABEL_MEASURED}</b> {s.rate_limit.measured.primary_used_percent.toFixed(0)}%
            {/if}
          </div>
        </div>
      {/each}
    </section>
  {/if}

  {#if data}
    {@const mo = data.month}
    {@const usedPct = mo.budget_tokens ? (mo.tokens / mo.budget_tokens) * 100 : 0}
    {@const projPct = mo.budget_tokens ? (mo.projected_tokens / mo.budget_tokens) * 100 : 0}
    <section class="chart-block">
      <h2>이번 달 <span class="muted">({mo.days_elapsed}/{mo.days_in_month}일)</span></h2>
      <div class="month-stats">
        <div class="ms">
          <span class="ms-label muted">사용</span>
          <span class="ms-val">{formatTokens(mo.tokens)} tok</span>
          {#if mo.cost_usd !== null}<span class="ms-sub muted">{formatCost(mo.cost_usd)}</span>{/if}
        </div>
        <div class="ms">
          <span class="ms-label muted">월말 예상</span>
          <span class="ms-val">{formatTokens(mo.projected_tokens)} tok</span>
          {#if mo.projected_cost_usd !== null}
            <span class="ms-sub muted">{formatCost(mo.projected_cost_usd)}</span>
          {/if}
        </div>
        {#if mo.budget_tokens}
          <div class="ms">
            <span class="ms-label muted">월 예산</span>
            <span class="ms-val">{formatTokens(mo.budget_tokens)} tok</span>
          </div>
        {/if}
      </div>
      {#if mo.budget_tokens}
        <div class="budget-bar">
          <span
            class="budget-fill"
            class:over={usedPct > 100}
            style={`width:${Math.min(100, usedPct)}%`}
          ></span>
          <span class="budget-proj" style={`left:${Math.min(100, projPct)}%`}></span>
        </div>
        <div class="muted small">
          예산의 {usedPct.toFixed(0)}% 사용 · 월말 예상 {projPct.toFixed(0)}%{projPct > 100
            ? " — 예산 초과 예상 ⚠"
            : ""}
        </div>
      {/if}
    </section>
  {/if}

  {#if scope === "all" && data && data.devices.length}
    <section class="chart-block">
      <h2>기기별 <span class="muted">(기간 합계)</span></h2>
      <div class="devices">
        {#each data.devices as d (d.hostname)}
          <div class="dev-row">
            <span class="dev-host">{d.hostname}{d.is_current ? " · 이 기기" : ""}</span>
            <span class="dev-tok">{formatTokens(d.tokens.total)} tok</span>
            <span class="dev-cost muted">
              {d.cost_usd === null ? LABEL_COST_NA : formatCost(d.cost_usd)}
            </span>
            <span class="dev-when muted">
              {d.is_current ? "실시간" : freshness(d.updated_at)}
            </span>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  {#if data && data.projects.length}
    {@const projMax = data.projects[0]?.tokens.total || 1}
    <section class="chart-block">
      <h2>프로젝트별 <span class="muted">(기간 합계)</span></h2>
      <div class="proj-legend muted">
        <span><span class="lg-dot" style={`background:${t.sources.claude_code}`}></span>Claude Code</span>
        <span><span class="lg-dot" style={`background:${t.sources.codex}`}></span>Codex</span>
      </div>
      <div class="projects">
        {#each data.projects.slice(0, 15) as p (p.project)}
          <div class="proj-row">
            <span
              class="proj-name"
              title={p.project === "(미분류)"
                ? "프로젝트(cwd)가 기록되지 않은 사용량 — 프로젝트 추적 이전의 과거 데이터가 대부분이며, 기간이 지나면 줄어듭니다"
                : p.project}>{p.project}</span>
            <span class="proj-bar">
              <span
                class="proj-fill"
                style={`width:${(p.claude_tokens / projMax) * 100}%;background:${t.sources.claude_code}`}
                title="Claude Code {formatTokens(p.claude_tokens)} tok"
              ></span>
              <span
                class="proj-fill"
                style={`width:${(p.codex_tokens / projMax) * 100}%;background:${t.sources.codex}`}
                title="Codex {formatTokens(p.codex_tokens)} tok"
              ></span>
            </span>
            <span class="proj-tok">{formatTokens(p.tokens.total)} tok</span>
            <span class="proj-cost muted">
              {p.cost_usd === null ? LABEL_COST_NA : formatCost(p.cost_usd)}
            </span>
          </div>
        {/each}
        {#if data.projects.length > 15}
          <div class="muted small">그 외 {data.projects.length - 15}개 프로젝트</div>
        {/if}
      </div>
    </section>
  {/if}

  <section class="grid-2">
    <div class="chart-block">
      <h2>도구별 추이 <span class="muted">(토큰)</span></h2>
      <div class="chart-wrap"><canvas bind:this={trendCanvas}></canvas></div>
    </div>
    <div class="chart-block">
      <h2>토큰 구성 <span class="muted">(입력·출력·캐시)</span></h2>
      <div class="chart-wrap"><canvas bind:this={compositionCanvas}></canvas></div>
    </div>
  </section>

  <section class="grid-2">
    <div class="chart-block">
      <h2>비용 추이 <span class="muted">({LABEL_COST}, 알려진 모델만)</span></h2>
      <div class="chart-wrap short"><canvas bind:this={costCanvas}></canvas></div>
    </div>
    <div class="chart-block">
      <h2>모델별 비교 <span class="muted">(기간 합계)</span></h2>
      <div class="chart-wrap short"><canvas bind:this={modelCanvas}></canvas></div>
    </div>
  </section>

  <section class="chart-block">
    <h2>사용 패턴 히트맵 <span class="muted">(요일×시간, 이 기기)</span></h2>
    <div class="heatmap">
      <div class="hm-corner"></div>
      {#each Array(24) as _, h}
        <div class="hm-hour">{h % 3 === 0 ? h : ""}</div>
      {/each}
      {#each WEEKDAYS as day, wd}
        <div class="hm-day">{day}</div>
        {#each Array(24) as _, h}
          <div
            class="hm-cell"
            style={`background:${heatColor(cellAt(wd, h))}`}
            title={`${day} ${h}시 — ${formatTokens(cellAt(wd, h))} tok`}
          ></div>
        {/each}
      {/each}
    </div>
  </section>

  {#if data}
    <footer class="muted">{data.timezone_note}</footer>
  {/if}
</div>

<style>
  .dashboard {
    padding: 1.25rem 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    box-sizing: border-box;
    height: 100%;
    overflow-y: auto;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  h1 {
    font-size: 1.1rem;
    margin: 0;
  }
  h2 {
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.01em;
    margin: 0 0 0.75rem;
    color: color-mix(in srgb, CanvasText 62%, transparent);
  }
  nav {
    display: flex;
    gap: 0.25rem;
  }
  nav button {
    font: inherit;
    font-size: 0.8rem;
    padding: 0.3rem 0.6rem;
    border-radius: 6px;
    border: 1px solid rgba(128, 128, 128, 0.35);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }
  nav button.active {
    background: #4f8ef7;
    border-color: #4f8ef7;
    color: white;
  }
  .scope-select {
    font: inherit;
    font-size: 0.8rem;
    padding: 0.3rem 0.5rem;
    border-radius: 6px;
    border: 1px solid rgba(128, 128, 128, 0.35);
    background: rgba(128, 128, 128, 0.12);
    color: inherit;
    cursor: pointer;
    max-width: 180px;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 0.75rem;
  }
  .card {
    border: 1px solid rgba(128, 128, 128, 0.25);
    border-left: 3px solid var(--accent);
    border-radius: 10px;
    padding: 0.7rem 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .card-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .card-title {
    font-size: 0.78rem;
    opacity: 0.75;
    display: inline-flex;
    align-items: center;
    gap: 0.35em;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 3px;
    background: var(--accent);
    display: inline-block;
  }
  .delta {
    font-size: 0.7rem;
    font-weight: 600;
  }
  .delta.up {
    color: #e34948;
  }
  .delta.down {
    color: #008300;
  }
  .delta.flat {
    opacity: 0.55;
  }
  .card-row {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    gap: 0.5rem;
  }
  .card-big {
    font-size: 1.55rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
  }
  .card-sub {
    font-size: 0.75rem;
    opacity: 0.75;
  }
  .saved {
    color: #008300;
    font-weight: 600;
  }
  .grid-2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
  }
  @media (max-width: 820px) {
    .grid-2 {
      grid-template-columns: 1fr;
    }
  }
  .chart-block {
    min-width: 0;
    border: 1px solid rgba(128, 128, 128, 0.22);
    border-radius: 12px;
    padding: 1rem 1.15rem;
    background: color-mix(in srgb, CanvasText 3%, transparent);
  }
  .chart-wrap {
    position: relative;
    height: 240px;
  }
  .chart-wrap.short {
    height: 200px;
  }
  .muted {
    opacity: 0.6;
    font-size: 0.72rem;
    font-weight: 400;
  }
  footer {
    margin-top: auto;
  }
  .nav-sep {
    width: 0.5rem;
  }
  .toast {
    font-size: 0.75rem;
    padding: 0.35rem 0.6rem;
    border-radius: 6px;
    border: 1px solid rgba(128, 128, 128, 0.3);
    opacity: 0.85;
    align-self: flex-start;
  }
  .devices {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .dev-row {
    display: grid;
    grid-template-columns: 1fr auto auto auto;
    gap: 0.75rem;
    align-items: baseline;
    padding: 0.3rem 0.1rem;
    border-bottom: 1px solid rgba(128, 128, 128, 0.12);
  }
  .dev-host {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dev-tok {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .dev-cost,
  .dev-when {
    font-size: 0.75rem;
  }
  .month-stats {
    display: flex;
    gap: 2rem;
    flex-wrap: wrap;
  }
  .ms {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .ms-label {
    font-size: 0.72rem;
  }
  .ms-val {
    font-variant-numeric: tabular-nums;
    font-weight: 700;
    font-size: 1.05rem;
  }
  .ms-sub {
    font-size: 0.75rem;
  }
  .budget-bar {
    position: relative;
    height: 10px;
    border-radius: 5px;
    background: rgba(128, 128, 128, 0.18);
    overflow: hidden;
    margin-top: 0.5rem;
  }
  .budget-fill {
    display: block;
    height: 100%;
    border-radius: 5px;
    background: #4f8ef7;
  }
  .budget-fill.over {
    background: #e0524f;
  }
  /* Month-end projection marker — a tick on the budget bar. */
  .budget-proj {
    position: absolute;
    top: 0;
    width: 2px;
    height: 100%;
    background: color-mix(in srgb, CanvasText 75%, transparent);
    transform: translateX(-1px);
  }
  .projects {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .proj-row {
    display: grid;
    grid-template-columns: minmax(6rem, 1.4fr) 3fr auto auto;
    gap: 0.75rem;
    align-items: center;
    padding: 0.3rem 0.1rem;
    border-bottom: 1px solid rgba(128, 128, 128, 0.12);
  }
  .proj-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .proj-bar {
    display: flex;
    height: 8px;
    border-radius: 4px;
    background: rgba(128, 128, 128, 0.18);
    overflow: hidden;
  }
  .proj-fill {
    height: 100%;
  }
  .proj-legend {
    display: flex;
    gap: 0.9rem;
    margin-bottom: 0.6rem;
    font-size: 0.72rem;
  }
  .proj-legend span {
    display: inline-flex;
    align-items: center;
    gap: 0.3em;
  }
  .lg-dot {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    display: inline-block;
  }
  .proj-tok {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    text-align: right;
  }
  .proj-cost {
    font-size: 0.75rem;
    text-align: right;
  }
  .heatmap {
    display: grid;
    grid-template-columns: 2rem repeat(24, 1fr);
    /* Thin seams between hour columns only; rows stay contiguous. */
    column-gap: 2px;
    row-gap: 0;
  }
  .hm-corner {
  }
  .hm-hour {
    font-size: 0.6rem;
    opacity: 0.55;
    text-align: center;
    padding-bottom: 4px;
  }
  .hm-day {
    font-size: 0.65rem;
    opacity: 0.65;
    display: flex;
    align-items: center;
    padding-right: 6px;
  }
  .hm-cell {
    /* Fixed height + stretch width: the cell always fills its grid column, so
       the tiling stays gapless at any window size. (aspect-ratio + max-height
       transferred the cap into a max-WIDTH on wide windows, leaving gaps.) */
    height: 22px;
    min-width: 0;
  }
  /* Round only the block's outer corners. */
  .hm-day + .hm-cell {
    border-top-left-radius: 4px;
    border-bottom-left-radius: 4px;
  }
  .hm-cell:last-child {
    border-top-right-radius: 4px;
    border-bottom-right-radius: 4px;
  }
</style>
