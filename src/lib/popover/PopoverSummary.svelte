<script lang="ts">
  import { onMount } from "svelte";
  import {
    getSummary,
    refreshNow,
    openDashboard,
    onUsageUpdated,
    type Summary,
    type SourceSummary,
  } from "../ipc";
  import {
    formatTokens,
    formatCost,
    formatResetTime,
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
  let refreshing = $state(false);

  onMount(() => {
    getSummary().then((s) => (summary = s));
    const unlisten = onUsageUpdated((s) => (summary = s));
    return () => {
      unlisten.then((fn) => fn());
    };
  });

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
</script>

<div class="popover">
  <header>
    <span class="app-name">meterly</span>
    <button class="ghost" onclick={doRefresh} disabled={refreshing}>
      {refreshing ? "…" : "↻"}
    </button>
  </header>

  {#if summary === null}
    <p class="muted center">불러오는 중…</p>
  {:else}
    {#each summary.sources as s (s.id)}
      <section class="source" style={`--accent:${t.sources[s.id] ?? "#8a8983"}`}>
        <div class="row top">
          <span class="name"><span class="dot"></span>{s.display_name}</span>
          {#if healthError(s)}
            <span class="warn" title={healthError(s)}
              >{LABEL_READ_ERROR} (포맷 미지원)</span
            >
          {:else}
            <span class="spark">
              <Sparkline
                values={s.last7_totals}
                color={t.sources[s.id] ?? "#8a8983"}
                width={56}
                height={18}
              />
            </span>
            <span class="tokens">{formatTokens(s.today_tokens.total)} tok</span>
          {/if}
        </div>
        {#if !healthError(s)}
          <div class="row detail">
            <span class="muted">
              in {formatTokens(s.today_tokens.input)} · out
              {formatTokens(s.today_tokens.output)} · cache
              {formatTokens(s.today_tokens.cache_read + s.today_tokens.cache_creation)}
            </span>
            <span class="cost" title="구독 요금이 아닌 API 정가 환산값">
              {LABEL_COST}
              {s.today_cost_usd === null ? LABEL_COST_NA : formatCost(s.today_cost_usd)}
              {#if s.today_cache_saved_usd !== null && s.today_cache_saved_usd >= 0.01}
                <span class="saved" title="캐시 읽기를 정가 입력으로 환산했을 때 대비 절약액">
                  캐시로 {formatCost(s.today_cache_saved_usd)} 절약
                </span>
              {/if}
            </span>
          </div>
          <div class="row limit">
            {#if s.rate_limit === "unavailable"}
              <span class="muted">한도 정보 없음</span>
            {:else if "estimated" in s.rate_limit}
              <span class="muted">
                <b class="badge">{LABEL_ESTIMATED}</b>
                {s.rate_limit.estimated.window_hours}시간 창
                {formatTokens(s.rate_limit.estimated.window_tokens)} tok ·
                리셋 {formatResetTime(s.rate_limit.estimated.resets_at)}
              </span>
            {:else if "measured" in s.rate_limit}
              <span class="muted">
                <b class="badge">{LABEL_MEASURED}</b>
                {s.rate_limit.measured.primary_used_percent.toFixed(0)}% 사용
                {#if s.rate_limit.measured.secondary_used_percent !== null}
                  (보조 {s.rate_limit.measured.secondary_used_percent.toFixed(0)}%)
                {/if}
                · 리셋 {formatResetTime(s.rate_limit.measured.resets_at)}
              </span>
              <span class="meter">
                <span
                  class="fill"
                  style={`width:${Math.min(100, s.rate_limit.measured.primary_used_percent)}%`}
                ></span>
              </span>
            {:else if "cli" in s.rate_limit}
              <div class="cli-usage">
                <span class="muted"><b class="badge">{LABEL_CLI}</b></span>
                {#if s.rate_limit.cli.session_percent !== null}
                  {@const p = s.rate_limit.cli.session_percent}
                  <div class="uwin" class:warn={p >= 70} class:crit={p >= 90}>
                    <span class="uwin-label">세션</span>
                    <span class="meter">
                      <span class="fill" style={`width:${Math.min(100, p)}%`}></span>
                    </span>
                    <span class="uwin-pct">{p.toFixed(0)}%</span>
                  </div>
                {/if}
                {#each s.rate_limit.cli.windows as w}
                  {@const p = w.used_percent}
                  <div class="uwin" class:warn={p >= 70} class:crit={p >= 90}>
                    <span class="uwin-label" title={w.label}>
                      {w.label === "all models" ? "주간" : `주간·${w.label}`}
                    </span>
                    <span class="meter">
                      <span class="fill" style={`width:${Math.min(100, p)}%`}></span>
                    </span>
                    <span class="uwin-pct">{p.toFixed(0)}%</span>
                    {#if w.resets_label}
                      <span class="muted small reset">리셋 {w.resets_label}</span>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          </div>
          {#if healthNote(s)}
            <div class="row"><span class="muted small">{healthNote(s)}</span></div>
          {/if}
        {/if}
      </section>
    {/each}
  {/if}

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
  }
  .app-name {
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .source {
    border: 1px solid var(--border, rgba(128, 128, 128, 0.25));
    border-radius: 10px;
    padding: 0.6rem 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
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
  .row.top .spark {
    margin-left: auto;
    display: inline-flex;
    align-items: flex-end;
  }
  .tokens {
    font-variant-numeric: tabular-nums;
    font-weight: 700;
    font-size: 1.05rem;
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
    gap: 0.1rem;
    font-size: 0.8rem;
    opacity: 0.85;
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
  .cli-usage {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 100%;
  }
  .uwin {
    display: flex;
    align-items: center;
    gap: 6px;
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
