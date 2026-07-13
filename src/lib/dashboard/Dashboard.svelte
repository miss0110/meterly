<script lang="ts">
  import { onMount } from "svelte";
  import {
    getDashboard,
    getSummary,
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
    data = await getDashboard(range);
    renderTrendChart(trendCanvas, data);
    renderCompositionChart(compositionCanvas, data);
    renderCostChart(costCanvas, data);
    renderModelChart(modelCanvas, data);
    heatmap = await getHeatmap();
  }

  const WEEKDAYS = ["월", "화", "수", "목", "금", "토", "일"];
  const heatmapMax = $derived(Math.max(...heatmap.map((c) => c.total), 1));

  function heatColor(total: number): string {
    if (total === 0) return "transparent";
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
    <h2>사용 패턴 히트맵 <span class="muted">(요일×시간, 전체 창 합계)</span></h2>
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
    padding: 1rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 1rem;
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
    font-size: 0.85rem;
    margin: 0 0 0.4rem;
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
  .heatmap {
    display: grid;
    grid-template-columns: 2rem repeat(24, 1fr);
    gap: 2px;
  }
  .hm-corner {
  }
  .hm-hour {
    font-size: 0.6rem;
    opacity: 0.55;
    text-align: center;
  }
  .hm-day {
    font-size: 0.65rem;
    opacity: 0.65;
    display: flex;
    align-items: center;
  }
  .hm-cell {
    aspect-ratio: 1 / 1.4;
    border-radius: 3px;
    border: 1px solid rgba(128, 128, 128, 0.12);
    min-width: 0;
  }
</style>
