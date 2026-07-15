<script lang="ts">
  import { onMount } from "svelte";
  import {
    getSettings,
    setTrayDisplay,
    setAutostart,
    setAlertsEnabled,
    setMonthlyBudget,
    setDateFormat,
    pickSyncFolder,
    clearSyncFolder,
    checkForUpdates,
    type SettingsData,
  } from "../ipc";

  let s = $state<SettingsData | null>(null);

  onMount(() => {
    getSettings().then((v) => (s = v));
  });

  const DISPLAY = [
    { id: "tokens", label: "사용량·비용 표시 (순환)" },
    { id: "icon", label: "아이콘만" },
  ];

  // Update the control immediately (optimistic), persist in the background —
  // the UI shouldn't wait on the IPC round-trip to reflect the choice.
  function chooseDisplay(mode: string) {
    if (s) s.tray_display = mode;
    void setTrayDisplay(mode);
  }
  function toggleAutostart(e: Event) {
    const enabled = (e.currentTarget as HTMLInputElement).checked;
    if (s) s.autostart = enabled;
    void setAutostart(enabled);
  }
  function toggleAlerts(e: Event) {
    const enabled = (e.currentTarget as HTMLInputElement).checked;
    if (s) s.alerts_enabled = enabled;
    void setAlertsEnabled(enabled);
  }
  const DATE_FORMATS = [
    { id: "auto", label: "자동 (지역 표준)" },
    { id: "iso", label: "2026-07-19 20:59" },
    { id: "us", label: "7/19 8:59 PM" },
    { id: "eu", label: "19/7 20:59" },
  ];
  function chooseDateFormat(e: Event) {
    const fmt = (e.currentTarget as HTMLSelectElement).value;
    if (s) s.date_format = fmt;
    void setDateFormat(fmt);
  }
  // Budget is entered/shown in millions of tokens; stored as raw tokens.
  function saveBudget(e: Event) {
    const m = parseFloat((e.currentTarget as HTMLInputElement).value);
    const tokens = Number.isFinite(m) && m > 0 ? Math.round(m * 1_000_000) : 0;
    if (s) s.monthly_budget_tokens = tokens > 0 ? tokens : null;
    void setMonthlyBudget(tokens);
  }
  async function pick() {
    const p = await pickSyncFolder();
    if (p && s) s.sync_dir = p;
  }
  async function clearSync() {
    await clearSyncFolder();
    if (s) s.sync_dir = null;
  }
</script>

<div class="settings">
  <h1>설정</h1>

  {#if s === null}
    <p class="muted">불러오는 중…</p>
  {:else}
    <section>
      <h2>트레이 아이콘 표현</h2>
      <div class="radios">
        {#each DISPLAY as d}
          <label class="radio">
            <input
              type="radio"
              name="display"
              checked={d.id === "icon" ? s.tray_display === "icon" : s.tray_display !== "icon"}
              onchange={() => chooseDisplay(d.id)}
            />
            {d.label}
          </label>
        {/each}
      </div>
    </section>

    <section>
      <h2>일반</h2>
      <label class="row-toggle">
        <input type="checkbox" checked={s.autostart} onchange={toggleAutostart} />
        로그인 시 자동 시작
      </label>
      <label class="row-toggle">
        <input type="checkbox" checked={s.alerts_enabled} onchange={toggleAlerts} />
        한도 사용률 알림 (30 · 50 · 70 · 90%)
      </label>
      <label class="row-select">
        날짜 표시 형식
        <select value={s.date_format} onchange={chooseDateFormat}>
          {#each DATE_FORMATS as f}
            <option value={f.id}>{f.label}</option>
          {/each}
        </select>
      </label>
    </section>

    <section>
      <h2>월 토큰 예산</h2>
      <p class="muted small">
        이번 달 사용량과 월말 예상치를 대시보드에서 이 예산과 비교합니다. 비우면 예산 없이
        사용량·예상치만 표시합니다.
      </p>
      <label class="budget">
        <input
          type="number"
          min="0"
          step="10"
          placeholder="예: 500"
          value={s.monthly_budget_tokens ? s.monthly_budget_tokens / 1_000_000 : ""}
          onchange={saveBudget}
        />
        <span class="unit">M tok / 월</span>
      </label>
    </section>

    <section>
      <h2>동기화 폴더</h2>
      <p class="muted small">
        여러 기기의 사용량을 합치려면, 모든 기기가 동기화하는 클라우드 폴더(iCloud /
        Google Drive / Dropbox / OneDrive)를 지정하세요.
      </p>
      <div class="path" class:unset={!s.sync_dir}>
        {s.sync_dir ?? "지정 안 됨 (이 기기만)"}
      </div>
      <div class="btn-row">
        <button onclick={pick}>폴더 선택…</button>
        {#if s.sync_dir}
          <button class="ghost" onclick={clearSync}>해제</button>
        {/if}
      </div>
    </section>

    <section>
      <h2>업데이트</h2>
      <div class="btn-row">
        <button onclick={() => checkForUpdates()}>업데이트 확인</button>
        <span class="muted ver">meterly v{s.version}</span>
      </div>
    </section>
  {/if}
</div>

<style>
  .settings {
    padding: 1.1rem 1.3rem;
    display: flex;
    flex-direction: column;
    gap: 1.1rem;
    height: 100%;
    box-sizing: border-box;
    font-size: 13px;
  }
  h1 {
    margin: 0;
    font-size: 1.1rem;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  h2 {
    margin: 0;
    font-size: 0.82rem;
    font-weight: 600;
    color: color-mix(in srgb, CanvasText 60%, transparent);
    text-transform: none;
  }
  .radios {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .radio,
  .row-toggle {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
  }
  .row-select {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
  }
  .row-select select {
    font: inherit;
    font-size: 0.85rem;
    padding: 0.25rem 0.4rem;
    border-radius: 7px;
    border: 1px solid color-mix(in srgb, CanvasText 25%, transparent);
    background: color-mix(in srgb, CanvasText 6%, transparent);
    color: inherit;
    cursor: pointer;
  }
  .muted {
    color: color-mix(in srgb, CanvasText 55%, transparent);
  }
  .small {
    font-size: 0.78rem;
    line-height: 1.4;
  }
  .path {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.78rem;
    padding: 0.5rem 0.6rem;
    border: 1px solid color-mix(in srgb, CanvasText 20%, transparent);
    border-radius: 7px;
    word-break: break-all;
  }
  .path.unset {
    color: color-mix(in srgb, CanvasText 45%, transparent);
    font-family: inherit;
  }
  .btn-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  button {
    padding: 0.35rem 0.8rem;
    border-radius: 7px;
    border: 1px solid color-mix(in srgb, CanvasText 25%, transparent);
    background: color-mix(in srgb, CanvasText 6%, transparent);
    color: inherit;
    cursor: pointer;
    font-size: 0.82rem;
  }
  button.ghost {
    background: transparent;
  }
  .ver {
    font-size: 0.78rem;
  }
  .budget {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .budget input {
    width: 7rem;
    padding: 0.35rem 0.5rem;
    border-radius: 7px;
    border: 1px solid color-mix(in srgb, CanvasText 25%, transparent);
    background: color-mix(in srgb, CanvasText 6%, transparent);
    color: inherit;
    font-size: 0.85rem;
  }
  .budget .unit {
    font-size: 0.8rem;
    color: color-mix(in srgb, CanvasText 55%, transparent);
  }
</style>
