<script lang="ts">
  import { onMount } from "svelte";
  import {
    getSettings,
    setTrayDisplay,
    setAutostart,
    setAlertsEnabled,
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

  async function chooseDisplay(mode: string) {
    await setTrayDisplay(mode);
    if (s) s.tray_display = mode;
  }
  async function toggleAutostart(e: Event) {
    const enabled = (e.currentTarget as HTMLInputElement).checked;
    await setAutostart(enabled);
    if (s) s.autostart = enabled;
  }
  async function toggleAlerts(e: Event) {
    const enabled = (e.currentTarget as HTMLInputElement).checked;
    await setAlertsEnabled(enabled);
    if (s) s.alerts_enabled = enabled;
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
</style>
