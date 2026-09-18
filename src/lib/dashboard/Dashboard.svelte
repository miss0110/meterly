<script lang="ts">
  import { onMount } from "svelte";
  import {
    getDashboard,
    getSummary,
    getDevices,
    getHeatmap,
    getSessions,
    exportData,
    onUsageUpdated,
    type DashboardData,
    type HeatmapCell,
    type SessionUsage,
    type SourceId,
    type TokenBreakdown,
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
    renderModelTrendChart,
    renderWeekCompareChart,
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
  // Heatmap tool filter: both tools, or one source.
  let heatSource = $state<SourceId | "all">("all");
  let sessions = $state<SessionUsage[]>([]);
  let sessionDay = $state<"today" | "yesterday">("today");
  let exportMsg = $state<string | null>(null);
  let trendCanvas: HTMLCanvasElement;
  let compositionCanvas: HTMLCanvasElement;
  let costCanvas: HTMLCanvasElement;
  let modelCanvas: HTMLCanvasElement;
  let modelTrendCanvas: HTMLCanvasElement;
  let weekCanvas: HTMLCanvasElement;

  const t = theme();

  async function load() {
    const d = await getDashboard(range, scope);
    data = d;
    // Each chart on its own: one renderer failing must not blank the rest
    // (or skip the heatmap/session loads below).
    const charts: [string, () => void][] = [
      ["trend", () => renderTrendChart(trendCanvas, d)],
      ["composition", () => renderCompositionChart(compositionCanvas, d)],
      ["cost", () => renderCostChart(costCanvas, d)],
      ["model", () => renderModelChart(modelCanvas, d)],
      ["model-trend", () => renderModelTrendChart(modelTrendCanvas, d)],
      ["week", () => renderWeekCompareChart(weekCanvas, d.weeks)],
    ];
    for (const [name, render] of charts) {
      try {
        render();
      } catch (e) {
        console.error(`chart ${name} failed`, e);
      }
    }
    heatmap = await getHeatmap(heatSource);
    await loadSessions();
  }

  /** Local calendar date `offset` days from today, YYYY-MM-DD. */
  function localDay(offset: number): string {
    const d = new Date();
    d.setDate(d.getDate() + offset);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }

  async function loadSessions() {
    sessions = await getSessions(localDay(sessionDay === "today" ? 0 : -1), 5);
  }

  function selectSessionDay(d: "today" | "yesterday") {
    sessionDay = d;
    loadSessions();
  }

  async function selectHeatSource(s: SourceId | "all") {
    heatSource = s;
    heatmap = await getHeatmap(s);
  }

  function clock(iso: string): string {
    const d = new Date(iso);
    return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  }

  /** "1h 20m" / "35m" / "<1m" span of a session. */
  function span(startIso: string, endIso: string): string {
    const m = Math.round((new Date(endIso).getTime() - new Date(startIso).getTime()) / 60_000);
    if (m < 1) return "<1m";
    if (m < 60) return `${m}m`;
    return `${Math.floor(m / 60)}h ${m % 60}m`;
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
    const k = Math.sqrt(total / heatmapMax);
    if (heatSource !== "all") {
      // Single tool: ramp in that tool's hue so the filter is legible at a glance.
      const pct = Math.round(18 + k * 82);
      return `color-mix(in srgb, ${t.sources[heatSource]} ${pct}%, transparent)`;
    }
    const idx = Math.min(t.seq.length - 1, 1 + Math.floor(k * (t.seq.length - 1)));
    return t.seq[idx];
  }

  const HEAT_SOURCES: { key: SourceId | "all"; label: string }[] = [
    { key: "all", label: "전체" },
    { key: "claude_code", label: "Claude" },
    { key: "codex", label: "Codex" },
  ];

  /** Week-over-week like-for-like caption (월 → today's weekday, both weeks). */
  function weekDelta(d: DashboardData): { text: string; dir: "up" | "down" | "flat" } | null {
    const w = d.weeks;
    if (w.last_to_date === 0) return w.this_to_date > 0 ? { text: "지난 주 같은 기간 사용 없음", dir: "flat" } : null;
    const pct = ((w.this_to_date - w.last_to_date) / w.last_to_date) * 100;
    const dir = pct > 5 ? "up" : pct < -5 ? "down" : "flat";
    return { text: `${pct > 0 ? "+" : ""}${pct.toFixed(0)}%`, dir };
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

  // ---- Derived insights (from data already in DashboardData / heatmap) ----

  /** Cache hit ratio: share of context tokens served from cache. */
  function cacheHit(tk: TokenBreakdown): number | null {
    const ctx = tk.input + tk.cache_read + tk.cache_creation;
    return ctx > 0 ? (tk.cache_read / ctx) * 100 : null;
  }

  const AVG_LABEL: Record<Range, string> = {
    daily30: "일평균",
    weekly12: "주평균",
    monthly6: "월평균",
  };

  /** Range-level KPIs: totals, per-period average, last vs previous period. */
  function kpis(d: DashboardData) {
    const periods = [...new Set(d.rows.map((r) => r.period))].sort();
    const per = periods.map((p) =>
      d.rows.filter((r) => r.period === p).reduce((n, r) => n + r.tokens.total, 0),
    );
    const tokens = per.reduce((a, b) => a + b, 0);
    let cost: number | null = null;
    for (const r of d.rows) if (r.cost_usd != null) cost = (cost ?? 0) + r.cost_usd;
    const avg = periods.length ? tokens / periods.length : 0;
    const last = per[per.length - 1] ?? 0;
    const prev = per[per.length - 2] ?? 0;
    const change = prev > 0 ? ((last - prev) / prev) * 100 : null;
    return { tokens, cost, avg, periods: periods.length, change };
  }

  /** Cost per model, ranked, with share of the period's known cost. */
  function modelCosts(d: DashboardData): { model: string; cost: number; share: number }[] {
    const by = new Map<string, number>();
    for (const r of d.rows) {
      if (r.cost_usd == null || r.cost_usd <= 0) continue;
      const k = r.model ?? "unknown";
      by.set(k, (by.get(k) ?? 0) + r.cost_usd);
    }
    const total = [...by.values()].reduce((a, b) => a + b, 0);
    return [...by.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, 6)
      .map(([model, cost]) => ({ model, cost, share: total ? (cost / total) * 100 : 0 }));
  }

  /** Busiest weekday × hour and the active hour span, for the heatmap caption. */
  const heatInsight = $derived.by(() => {
    if (!heatmap.length || heatmapMax <= 1) return null;
    const peak = heatmap.reduce((a, c) => (c.total > a.total ? c : a));
    const hours = heatmap.filter((c) => c.total > 0).map((c) => c.hour);
    const span = hours.length ? `${Math.min(...hours)}–${Math.max(...hours)}시` : null;
    return { day: WEEKDAYS[peak.weekday], hour: peak.hour, span };
  });
</script>

<div class="dashboard">
  <header>
    <h1><span class="orb" aria-hidden="true"></span>meterly <span class="h1-sub">대시보드</span></h1>
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
      {/if}
      <div class="seg" role="tablist" aria-label="기간">
        {#each RANGES as r (r.key)}
          <button
            role="tab"
            aria-selected={range === r.key}
            class:active={range === r.key}
            onclick={() => selectRange(r.key)}
          >
            {r.label}
          </button>
        {/each}
      </div>
      <div class="seg ghost">
        <button title="현재 범위 집계를 CSV로 저장" onclick={() => doExport("csv")}>CSV</button>
        <button title="현재 범위 집계를 JSON으로 저장" onclick={() => doExport("json")}>JSON</button>
      </div>
    </nav>
  </header>
  {#if exportMsg}
    <div class="toast">{exportMsg}</div>
  {/if}

  {#if summary}
    <section class="cards">
      {#each summary.sources as s (s.id)}
        {@const hit = cacheHit(s.today_tokens)}
        <div class="card" style={`--accent:${t.sources[s.id] ?? "#8a8983"}`}>
          <div class="card-head">
            <span class="card-title">
              <span class="dot"></span>
              {SOURCE_LABELS[s.id] ?? s.display_name}
              <span class="card-when">오늘</span>
            </span>
            <span class="delta {delta(s.last7_totals).dir}" title="전일 대비">
              {delta(s.last7_totals).text}
            </span>
          </div>
          <div class="card-row">
            <span class="card-big">{formatTokens(s.today_tokens.total)}<span class="unit">tok</span></span>
            <Sparkline
              values={s.last7_totals}
              color={t.sources[s.id] ?? "#8a8983"}
              width={132}
              height={30}
            />
          </div>
          <div class="chips">
            <span class="chip" title="구독 요금이 아닌 API 정가 환산값">
              <span class="chip-k">{LABEL_COST}</span>
              {s.today_cost_usd === null ? LABEL_COST_NA : formatCost(s.today_cost_usd)}
            </span>
            {#if s.today_cache_saved_usd !== null && s.today_cache_saved_usd >= 0.01}
              <span class="chip good" title="캐시 읽기를 정가 입력으로 환산했을 때 대비 절약액">
                캐시 {formatCost(s.today_cache_saved_usd)} 절약
              </span>
            {/if}
            {#if hit !== null}
              <span class="chip" title="컨텍스트 토큰 중 캐시에서 읽은 비율 — 높을수록 절약이 큽니다">
                <span class="chip-k">캐시 적중</span>{hit.toFixed(0)}%
              </span>
            {/if}
            {#if s.rate_limit !== "unavailable" && "estimated" in s.rate_limit}
              <span class="chip"><span class="chip-k">{LABEL_ESTIMATED}</span>{formatTokens(s.rate_limit.estimated.window_tokens)}</span>
            {:else if s.rate_limit !== "unavailable" && "measured" in s.rate_limit}
              <span class="chip live"><span class="chip-k">{LABEL_MEASURED}</span>{s.rate_limit.measured.primary_used_percent.toFixed(0)}% 사용</span>
            {/if}
          </div>
        </div>
      {/each}
    </section>
  {/if}

  {#if data}
    {@const k = kpis(data)}
    <section class="kpis">
      <div class="kpi">
        <span class="kpi-k">기간 합계</span>
        <span class="kpi-v">{formatTokens(k.tokens)}<span class="unit">tok</span></span>
        <span class="kpi-s">{RANGES.find((r) => r.key === range)?.label} · {k.periods}구간</span>
      </div>
      <div class="kpi">
        <span class="kpi-k">기간 비용</span>
        <span class="kpi-v">{k.cost === null ? LABEL_COST_NA : formatCost(k.cost)}</span>
        <span class="kpi-s">{LABEL_COST}, 알려진 모델만</span>
      </div>
      <div class="kpi">
        <span class="kpi-k">{AVG_LABEL[range]}</span>
        <span class="kpi-v">{formatTokens(k.avg)}<span class="unit">tok</span></span>
        <span class="kpi-s">구간당 평균</span>
      </div>
      <div class="kpi">
        <span class="kpi-k">이전 구간 대비</span>
        {#if k.change === null}
          <span class="kpi-v muted">—</span>
        {:else}
          <span class="kpi-v {k.change > 5 ? 'up' : k.change < -5 ? 'down' : ''}">
            {k.change > 0 ? "+" : ""}{k.change.toFixed(0)}%
          </span>
        {/if}
        <span class="kpi-s">마지막 구간 vs 그 이전</span>
      </div>
    </section>
  {/if}

  {#if data}
    {@const mo = data.month}
    {@const usedPct = mo.budget_tokens ? (mo.tokens / mo.budget_tokens) * 100 : 0}
    {@const projPct = mo.budget_tokens ? (mo.projected_tokens / mo.budget_tokens) * 100 : 0}
    <section class="panel">
      <h2>이번 달 <span class="muted">{mo.days_elapsed}/{mo.days_in_month}일</span></h2>
      <div class="month-stats">
        <div class="ms">
          <span class="ms-label">사용</span>
          <span class="ms-val">{formatTokens(mo.tokens)}<span class="unit">tok</span></span>
          {#if mo.cost_usd !== null}<span class="ms-sub">{formatCost(mo.cost_usd)}</span>{/if}
        </div>
        <div class="ms">
          <span class="ms-label">월말 예상</span>
          <span class="ms-val">{formatTokens(mo.projected_tokens)}<span class="unit">tok</span></span>
          {#if mo.projected_cost_usd !== null}
            <span class="ms-sub">{formatCost(mo.projected_cost_usd)}</span>
          {/if}
        </div>
        {#if mo.budget_tokens}
          <div class="ms">
            <span class="ms-label">월 예산</span>
            <span class="ms-val">{formatTokens(mo.budget_tokens)}<span class="unit">tok</span></span>
            <span class="ms-sub {projPct > 100 ? 'over' : ''}">
              {usedPct.toFixed(0)}% 사용 · 월말 {projPct.toFixed(0)}%{projPct > 100 ? " — 초과 예상" : ""}
            </span>
          </div>
        {/if}
      </div>
      {#if mo.budget_tokens}
        <div class="budget-bar" class:over={usedPct > 100}>
          <span class="budget-fill" style={`width:${Math.min(100, usedPct)}%`}></span>
          <span class="budget-proj" style={`left:${Math.min(100, projPct)}%`} title="월말 예상"></span>
        </div>
      {/if}
    </section>
  {/if}

  <section class="panel">
    <div class="panel-head">
      <h2>가장 비싼 세션 <span class="muted">Top 5 · 이 기기 · {LABEL_COST}</span></h2>
      <div class="seg mini" role="tablist" aria-label="세션 날짜">
        <button role="tab" aria-selected={sessionDay === "today"} class:active={sessionDay === "today"} onclick={() => selectSessionDay("today")}>오늘</button>
        <button role="tab" aria-selected={sessionDay === "yesterday"} class:active={sessionDay === "yesterday"} onclick={() => selectSessionDay("yesterday")}>어제</button>
      </div>
    </div>
    {#if sessions.length}
      <div class="rows">
        {#each sessions as s, i (s.source + s.session_id)}
          <div class="sess-row" style={`--accent:${t.sources[s.source] ?? "#8a8983"}`}>
            <span class="rank">{i + 1}</span>
            <span class="r-name" title={s.project ?? "프로젝트 미기록"}>
              <span class="src-dot" title={SOURCE_LABELS[s.source]}></span>
              {s.project ?? "(미분류)"}
            </span>
            <span class="r-name mono" title={s.model_count > 1 ? `${s.model_count}개 모델 사용 — 가장 많이 쓴 모델 표시` : ""}>
              {s.model ?? "unknown"}{#if s.model_count > 1}<span class="plus">+{s.model_count - 1}</span>{/if}
            </span>
            <span class="r-sub" title="세션 시작 – 마지막 응답">{clock(s.first_at)}–{clock(s.last_at)} · {span(s.first_at, s.last_at)}</span>
            <span class="r-num">{formatTokens(s.tokens.total)}<span class="unit">tok</span></span>
            <span class="r-cost">{s.cost_usd === null ? LABEL_COST_NA : formatCost(s.cost_usd)}</span>
          </div>
        {/each}
      </div>
    {:else}
      <p class="muted small empty">{sessionDay === "today" ? "오늘" : "어제"} 기록된 세션이 없습니다.</p>
    {/if}
  </section>

  {#if scope === "all" && data && data.devices.length}
    <section class="panel">
      <h2>기기별 <span class="muted">기간 합계</span></h2>
      <div class="rows">
        {#each data.devices as d (d.hostname)}
          <div class="row4">
            <span class="r-name">{d.hostname}{#if d.is_current}<span class="here">이 기기</span>{/if}</span>
            <span class="r-num">{formatTokens(d.tokens.total)}<span class="unit">tok</span></span>
            <span class="r-sub">{d.cost_usd === null ? LABEL_COST_NA : formatCost(d.cost_usd)}</span>
            <span class="r-sub">{d.is_current ? "실시간" : freshness(d.updated_at)}</span>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  {#if data && data.projects.length}
    {@const projMax = data.projects[0]?.tokens.total || 1}
    <section class="panel">
      <div class="panel-head">
        <h2>프로젝트별 <span class="muted">기간 합계</span></h2>
        <div class="legend">
          <span><i style={`background:${t.sources.claude_code}`}></i>Claude Code</span>
          <span><i style={`background:${t.sources.codex}`}></i>Codex</span>
        </div>
      </div>
      <div class="rows">
        {#each data.projects.slice(0, 15) as p (p.project)}
          <div class="proj-row">
            <span
              class="r-name"
              title={p.project === "(미분류)"
                ? "프로젝트(cwd)가 기록되지 않은 사용량 — 프로젝트 추적 이전의 과거 데이터가 대부분이며, 기간이 지나면 줄어듭니다"
                : p.project}>{p.project}</span>
            <span class="bar">
              <span
                class="bar-fill"
                style={`width:${(p.claude_tokens / projMax) * 100}%;background:${t.sources.claude_code}`}
                title="Claude Code {formatTokens(p.claude_tokens)} tok"
              ></span>
              <span
                class="bar-fill"
                style={`width:${(p.codex_tokens / projMax) * 100}%;background:${t.sources.codex}`}
                title="Codex {formatTokens(p.codex_tokens)} tok"
              ></span>
            </span>
            <span class="r-num">{formatTokens(p.tokens.total)}<span class="unit">tok</span></span>
            <span class="r-sub">{p.cost_usd === null ? LABEL_COST_NA : formatCost(p.cost_usd)}</span>
          </div>
        {/each}
        {#if data.projects.length > 15}
          <div class="muted small">그 외 {data.projects.length - 15}개 프로젝트</div>
        {/if}
      </div>
    </section>
  {/if}

  <section class="grid-2">
    <div class="panel">
      <h2>도구별 추이 <span class="muted">토큰</span></h2>
      <div class="chart-wrap"><canvas bind:this={trendCanvas}></canvas></div>
    </div>
    <div class="panel">
      <h2>토큰 구성 <span class="muted">입력·출력·캐시</span></h2>
      <div class="chart-wrap"><canvas bind:this={compositionCanvas}></canvas></div>
    </div>
  </section>

  <section class="grid-2">
    <div class="panel">
      <h2>모델별 추이 <span class="muted">어떤 모델로 옮겨갔는지</span></h2>
      <div class="chart-wrap"><canvas bind:this={modelTrendCanvas}></canvas></div>
    </div>
    <div class="panel">
      <div class="panel-head">
        <h2>이번 주 vs 지난 주 <span class="muted">요일별</span></h2>
        {#if data}
          {@const wd = weekDelta(data)}
          {#if wd}
            <span class="delta {wd.dir}" title="월요일부터 오늘 요일까지, 두 주를 같은 기간으로 비교">{wd.text} vs 지난 주</span>
          {/if}
        {/if}
      </div>
      <div class="chart-wrap"><canvas bind:this={weekCanvas}></canvas></div>
      {#if data}
        {@const w = data.weeks}
        <p class="insight">
          월–{WEEKDAYS[w.today_weekday]} 기준 이번 주 <b>{formatTokens(w.this_to_date)}</b>
          · 지난 주 같은 기간 <b>{formatTokens(w.last_to_date)}</b>
          {#if w.this_cost !== null || w.last_cost !== null}
            · {LABEL_COST} {formatCost(w.this_cost ?? 0)} / 지난 주 전체 {formatCost(w.last_cost ?? 0)}
          {/if}
        </p>
      {/if}
    </div>
  </section>

  <section class="grid-2">
    <div class="panel">
      <h2>비용 추이 <span class="muted">{LABEL_COST}, 알려진 모델만</span></h2>
      <div class="chart-wrap short"><canvas bind:this={costCanvas}></canvas></div>
    </div>
    <div class="panel">
      <h2>모델별 비교 <span class="muted">기간 합계</span></h2>
      <div class="chart-wrap short"><canvas bind:this={modelCanvas}></canvas></div>
    </div>
  </section>

  {#snippet heatmapPanel()}
    <div class="panel">
      <div class="panel-head">
        <h2>사용 패턴 <span class="muted">요일×시간, 이 기기</span></h2>
        <div class="seg mini" role="tablist" aria-label="히트맵 도구">
          {#each HEAT_SOURCES as hs (hs.key)}
            <button
              role="tab"
              aria-selected={heatSource === hs.key}
              class:active={heatSource === hs.key}
              style={hs.key !== "all" ? `--seg-accent:${t.sources[hs.key]}` : ""}
              onclick={() => selectHeatSource(hs.key)}
            >
              {hs.label}
            </button>
          {/each}
        </div>
      </div>
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
      {#if heatInsight}
        <p class="insight">
          가장 활발한 시간 <b>{heatInsight.day} {heatInsight.hour}시</b>
          {#if heatInsight.span}· 활동 시간대 <b>{heatInsight.span}</b>{/if}
          {#if heatSource !== "all"}<span class="muted"> · {SOURCE_LABELS[heatSource]}만</span>{/if}
        </p>
      {:else if heatSource !== "all"}
        <p class="insight muted">{SOURCE_LABELS[heatSource]} 사용 기록이 없습니다.</p>
      {/if}
    </div>
  {/snippet}

  {#if data}
    {@const mc = modelCosts(data)}
    {#if mc.length}
      <section class="grid-2">
        <div class="panel">
          <h2>모델별 비용 비중 <span class="muted">기간 합계</span></h2>
          <div class="rows">
            {#each mc as m (m.model)}
              <div class="mc-row">
                <span class="r-name mono">{m.model}</span>
                <span class="bar"><span class="bar-fill accent" style={`width:${m.share}%`}></span></span>
                <span class="r-num">{formatCost(m.cost)}</span>
                <span class="r-sub">{m.share.toFixed(0)}%</span>
              </div>
            {/each}
          </div>
        </div>
        {@render heatmapPanel()}
      </section>
    {:else}
      <section>{@render heatmapPanel()}</section>
    {/if}
  {/if}

  {#if data}
    <footer class="muted">{data.timezone_note}</footer>
  {/if}
</div>

<style>
  /* ---------- Shell ---------- */
  .dashboard {
    padding: 1.15rem 1.4rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    box-sizing: border-box;
    height: 100%;
    overflow-y: auto;
    background:
      radial-gradient(
        70% 45% at 50% -10%,
        color-mix(in srgb, #4f8ef7 12%, transparent),
        transparent 65%
      );
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
    flex-wrap: wrap;
  }
  h1 {
    display: inline-flex;
    align-items: center;
    gap: 0.55rem;
    font-size: 1.1rem;
    font-weight: 800;
    letter-spacing: -0.01em;
    margin: 0;
  }
  .h1-sub {
    font-weight: 500;
    opacity: 0.45;
    font-size: 0.9rem;
  }
  .orb {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: linear-gradient(135deg, #eb6834, #4f8ef7);
    box-shadow: 0 0 12px color-mix(in srgb, #4f8ef7 55%, transparent);
  }
  nav {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  /* Segmented control: one pill housing the range tabs. */
  .seg {
    display: inline-flex;
    padding: 3px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 7%, transparent);
    border: 1px solid color-mix(in srgb, currentColor 9%, transparent);
  }
  .seg button {
    font: inherit;
    font-size: 0.78rem;
    font-weight: 600;
    padding: 0.28rem 0.75rem;
    border-radius: 999px;
    border: none;
    background: transparent;
    color: inherit;
    opacity: 0.7;
    cursor: pointer;
    transition: background 0.15s ease, opacity 0.15s ease;
  }
  .seg button:hover {
    opacity: 1;
  }
  .seg button.active {
    opacity: 1;
    color: #fff;
    background: linear-gradient(135deg, #4f8ef7, #6a5cf5);
    box-shadow: 0 6px 14px -8px rgba(79, 142, 247, 0.8);
  }
  .seg.ghost button {
    font-weight: 500;
    padding: 0.28rem 0.6rem;
  }
  /* Compact segmented control inside a panel header. */
  .seg.mini {
    padding: 2px;
  }
  .seg.mini button {
    font-size: 0.68rem;
    padding: 0.18rem 0.6rem;
  }
  .seg.mini button.active {
    background: linear-gradient(
      135deg,
      var(--seg-accent, #4f8ef7),
      color-mix(in srgb, var(--seg-accent, #6a5cf5) 70%, #6a5cf5)
    );
    box-shadow: 0 6px 14px -8px color-mix(in srgb, var(--seg-accent, #4f8ef7) 80%, transparent);
  }
  .panel-head .seg {
    margin-bottom: 0.8rem;
  }
  .scope-select {
    font: inherit;
    font-size: 0.78rem;
    padding: 0.32rem 0.7rem;
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, currentColor 14%, transparent);
    background: color-mix(in srgb, currentColor 7%, transparent);
    color: inherit;
    cursor: pointer;
    max-width: 180px;
  }
  .toast {
    font-size: 0.75rem;
    padding: 0.4rem 0.75rem;
    border-radius: 999px;
    background: color-mix(in srgb, #4f8ef7 14%, transparent);
    border: 1px solid color-mix(in srgb, #4f8ef7 35%, transparent);
    align-self: flex-start;
  }

  /* ---------- Glass panels (shared by cards and chart blocks) ---------- */
  .panel,
  .card,
  .kpi {
    min-width: 0;
    border-radius: 16px;
    background: color-mix(in srgb, currentColor 4%, transparent);
    border: 1px solid color-mix(in srgb, currentColor 9%, transparent);
    box-shadow: inset 0 1px 0 color-mix(in srgb, #fff 6%, transparent);
  }
  .panel {
    padding: 0.95rem 1.1rem 1rem;
  }
  .panel-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
  }
  h2 {
    font-size: 0.66rem;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    margin: 0 0 0.8rem;
    opacity: 0.55;
  }
  h2 .muted {
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
    margin-left: 0.4em;
  }
  .muted {
    opacity: 0.55;
    font-size: 0.74rem;
    font-weight: 400;
  }
  .small {
    font-size: 0.72rem;
  }
  .unit {
    font-size: 0.62em;
    font-weight: 600;
    letter-spacing: 0.04em;
    opacity: 0.5;
    margin-left: 0.3em;
  }

  /* ---------- Today cards ---------- */
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 0.9rem;
  }
  .card {
    position: relative;
    padding: 0.9rem 1.05rem 0.95rem;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    background:
      radial-gradient(
        90% 60% at 0% 0%,
        color-mix(in srgb, var(--accent) 16%, transparent),
        transparent 70%
      ),
      color-mix(in srgb, currentColor 4.5%, transparent);
    border-color: color-mix(in srgb, var(--accent) 26%, transparent);
    box-shadow:
      inset 0 1px 0 color-mix(in srgb, #fff 7%, transparent),
      0 12px 32px -20px color-mix(in srgb, var(--accent) 50%, transparent);
  }
  .card-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .card-title {
    font-size: 0.98rem;
    font-weight: 700;
    letter-spacing: -0.01em;
    display: inline-flex;
    align-items: center;
    gap: 0.5em;
  }
  .card-when {
    font-size: 0.66rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    opacity: 0.45;
    margin-left: 0.2em;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--accent);
    display: inline-block;
    box-shadow:
      0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent),
      0 0 12px color-mix(in srgb, var(--accent) 60%, transparent);
  }
  .delta {
    font-size: 0.7rem;
    font-weight: 700;
    padding: 2px 9px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 8%, transparent);
  }
  .delta.up {
    color: #e0524f;
    background: color-mix(in srgb, #e0524f 14%, transparent);
  }
  .delta.down {
    color: #22a559;
    background: color-mix(in srgb, #22a559 14%, transparent);
  }
  .delta.flat {
    opacity: 0.6;
  }
  .card-row {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    gap: 0.75rem;
  }
  .card-big {
    font-size: 2rem;
    font-weight: 800;
    letter-spacing: -0.03em;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 0.72rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    padding: 3px 10px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 7%, transparent);
    white-space: nowrap;
  }
  .chip-k {
    font-weight: 500;
    opacity: 0.6;
  }
  .chip.good {
    color: #22a559;
    background: color-mix(in srgb, #22a559 14%, transparent);
  }
  .chip.live {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .chip.live .chip-k {
    color: inherit;
    opacity: 0.75;
  }

  /* ---------- Range KPIs ---------- */
  .kpis {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 0.75rem;
  }
  .kpi {
    padding: 0.75rem 0.95rem;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .kpi-k {
    font-size: 0.62rem;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    opacity: 0.5;
  }
  .kpi-v {
    font-size: 1.3rem;
    font-weight: 800;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
  }
  .kpi-v.up { color: #e0524f; }
  .kpi-v.down { color: #22a559; }
  .kpi-v.muted { font-size: 1.3rem; opacity: 0.35; }
  .kpi-s {
    font-size: 0.7rem;
    opacity: 0.5;
  }

  /* ---------- Month ---------- */
  .month-stats {
    display: flex;
    gap: 2.25rem;
    flex-wrap: wrap;
  }
  .ms {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .ms-label {
    font-size: 0.62rem;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    opacity: 0.5;
  }
  .ms-val {
    font-variant-numeric: tabular-nums;
    font-weight: 800;
    font-size: 1.25rem;
    letter-spacing: -0.02em;
    line-height: 1.1;
  }
  .ms-sub {
    font-size: 0.74rem;
    opacity: 0.6;
  }
  .ms-sub.over {
    color: #e0524f;
    opacity: 1;
    font-weight: 600;
  }
  .budget-bar {
    position: relative;
    height: 10px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 10%, transparent);
    margin-top: 0.85rem;
  }
  .budget-fill {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: linear-gradient(90deg, #4f8ef7, #7aa9ff);
    box-shadow: 0 0 12px color-mix(in srgb, #4f8ef7 55%, transparent);
    transition: width 0.4s ease;
  }
  .budget-bar.over .budget-fill {
    background: linear-gradient(90deg, #e0524f, #ff8a7a);
    box-shadow: 0 0 12px color-mix(in srgb, #e0524f 60%, transparent);
  }
  /* Month-end projection: a diamond tick riding on the bar. */
  .budget-proj {
    position: absolute;
    top: 50%;
    width: 10px;
    height: 10px;
    border-radius: 2px;
    background: CanvasText;
    transform: translate(-50%, -50%) rotate(45deg);
    box-shadow: 0 0 0 2px Canvas;
  }

  /* ---------- Row lists (devices / projects / model cost) ---------- */
  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row4,
  .proj-row,
  .mc-row {
    display: grid;
    gap: 0.9rem;
    align-items: center;
    padding: 0.42rem 0.55rem;
    border-radius: 10px;
    font-size: 0.86rem;
  }
  .row4 { grid-template-columns: 1fr auto auto auto; }
  .sess-row {
    display: grid;
    grid-template-columns: 1.4rem minmax(6rem, 1.2fr) minmax(8rem, 1.2fr) auto auto 5rem;
    gap: 0.9rem;
    align-items: center;
    padding: 0.42rem 0.55rem;
    border-radius: 10px;
    font-size: 0.86rem;
  }
  .sess-row:hover {
    background: color-mix(in srgb, currentColor 5%, transparent);
  }
  .rank {
    font-size: 0.7rem;
    font-weight: 800;
    font-variant-numeric: tabular-nums;
    opacity: 0.45;
    text-align: center;
  }
  .src-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .plus {
    font-size: 0.62rem;
    font-weight: 700;
    opacity: 0.55;
    margin-left: 0.35em;
    padding: 0 5px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 9%, transparent);
  }
  .r-cost {
    font-variant-numeric: tabular-nums;
    font-weight: 800;
    text-align: right;
    white-space: nowrap;
    color: #4f8ef7;
  }
  .empty {
    margin: 0.2rem 0 0;
  }
  .proj-row { grid-template-columns: minmax(7rem, 1.3fr) 3fr auto auto; }
  .mc-row { grid-template-columns: minmax(9rem, 1.4fr) 3fr auto 3.2rem; }
  .row4:hover,
  .proj-row:hover,
  .mc-row:hover {
    background: color-mix(in srgb, currentColor 5%, transparent);
  }
  .r-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
    display: inline-flex;
    align-items: center;
    gap: 0.5em;
    min-width: 0;
  }
  .r-name.mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8rem;
    font-weight: 500;
  }
  .here {
    font-size: 0.62rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    color: #22a559;
    background: color-mix(in srgb, #22a559 14%, transparent);
    padding: 1px 7px;
    border-radius: 999px;
  }
  .r-num {
    font-variant-numeric: tabular-nums;
    font-weight: 700;
    text-align: right;
    white-space: nowrap;
  }
  .r-sub {
    font-size: 0.74rem;
    opacity: 0.55;
    text-align: right;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .bar {
    display: flex;
    gap: 2px;
    height: 8px;
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 8%, transparent);
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    border-radius: 999px;
    transition: width 0.4s ease;
  }
  .bar-fill.accent {
    background: linear-gradient(90deg, #4f8ef7, #7aa9ff);
    box-shadow: 0 0 8px color-mix(in srgb, #4f8ef7 45%, transparent);
  }
  .legend {
    display: flex;
    gap: 0.9rem;
    font-size: 0.7rem;
    opacity: 0.7;
  }
  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 0.35em;
  }
  .legend i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    display: inline-block;
  }

  /* ---------- Charts ---------- */
  .grid-2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.9rem;
  }
  @media (max-width: 820px) {
    .grid-2 {
      grid-template-columns: 1fr;
    }
  }
  .chart-wrap {
    position: relative;
    height: 230px;
  }
  .chart-wrap.short {
    height: 200px;
  }

  /* ---------- Heatmap ---------- */
  .heatmap {
    display: grid;
    grid-template-columns: 1.8rem repeat(24, 1fr);
    gap: 3px;
  }
  .hm-hour {
    font-size: 0.58rem;
    opacity: 0.5;
    text-align: center;
    padding-bottom: 2px;
    font-variant-numeric: tabular-nums;
  }
  .hm-day {
    font-size: 0.66rem;
    opacity: 0.6;
    display: flex;
    align-items: center;
    font-weight: 600;
  }
  .hm-cell {
    height: 18px;
    min-width: 0;
    border-radius: 4px;
    transition: transform 0.1s ease;
  }
  .hm-cell:hover {
    transform: scale(1.25);
    outline: 1px solid color-mix(in srgb, CanvasText 40%, transparent);
    z-index: 1;
    position: relative;
  }
  .insight {
    margin: 0.75rem 0 0;
    font-size: 0.78rem;
    opacity: 0.75;
  }
  .insight b {
    font-weight: 700;
    opacity: 1;
  }

  footer {
    margin-top: auto;
    padding-top: 0.25rem;
  }
</style>
