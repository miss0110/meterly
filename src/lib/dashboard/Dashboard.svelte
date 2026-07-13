<script lang="ts">
  import { onMount } from "svelte";
  import {
    getDashboard,
    getSummary,
    onUsageUpdated,
    type DashboardData,
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
  import { renderTrendChart, renderModelChart } from "./charts";

  const RANGES: { key: Range; label: string }[] = [
    { key: "daily30", label: "일 (30일)" },
    { key: "weekly12", label: "주 (12주)" },
    { key: "monthly6", label: "월 (6개월)" },
  ];

  let range = $state<Range>("daily30");
  let data = $state<DashboardData | null>(null);
  let summary = $state<Summary | null>(null);
  let trendCanvas: HTMLCanvasElement;
  let modelCanvas: HTMLCanvasElement;

  async function load() {
    data = await getDashboard(range);
    renderTrendChart(trendCanvas, data);
    renderModelChart(modelCanvas, data);
  }

  onMount(() => {
    getSummary().then((s) => (summary = s));
    load();
    const unlisten = onUsageUpdated((s) => {
      summary = s;
      load();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  });

  function selectRange(r: Range) {
    range = r;
    load();
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
    </nav>
  </header>

  {#if summary}
    <section class="cards">
      {#each summary.sources as s (s.id)}
        <div class="card">
          <div class="card-title">{s.display_name} · 오늘</div>
          <div class="card-big">{formatTokens(s.today_tokens.total)} tok</div>
          <div class="card-sub">
            {LABEL_COST}
            {s.today_cost_usd === null ? LABEL_COST_NA : formatCost(s.today_cost_usd)}
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

  <section class="chart-block">
    <h2>추이 <span class="muted">(도구별 누적, 토큰)</span></h2>
    <div class="chart-wrap"><canvas bind:this={trendCanvas}></canvas></div>
  </section>

  <section class="chart-block">
    <h2>모델별 비교 <span class="muted">(기간 합계, 토큰)</span></h2>
    <div class="chart-wrap short"><canvas bind:this={modelCanvas}></canvas></div>
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
    font-size: 0.9rem;
    margin: 0 0 0.5rem;
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
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 0.75rem;
  }
  .card {
    border: 1px solid rgba(128, 128, 128, 0.25);
    border-radius: 10px;
    padding: 0.75rem;
  }
  .card-title {
    font-size: 0.8rem;
    opacity: 0.7;
  }
  .card-big {
    font-size: 1.5rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .card-sub {
    font-size: 0.78rem;
    opacity: 0.75;
  }
  .chart-wrap {
    position: relative;
    height: 260px;
  }
  .chart-wrap.short {
    height: 200px;
  }
  .muted {
    opacity: 0.6;
    font-size: 0.75rem;
    font-weight: 400;
  }
  footer {
    margin-top: auto;
  }
</style>
