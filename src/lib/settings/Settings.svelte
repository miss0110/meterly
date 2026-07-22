<script lang="ts">
  import { onMount } from "svelte";
  import {
    getSettings,
    setTrayDisplay,
    setAutostart,
    setAlertsEnabled,
    setAlertThresholds,
    setPercentDisplay,
    setMonthlyBudget,
    setDateFormat,
    pickSyncFolder,
    clearSyncFolder,
    checkForUpdates,
    openLogDir,
    getOrgStatus,
    setOrgConfig,
    setOrgSources,
    orgRegister,
    orgReportNow,
    orgDisable,
    type SettingsData,
    type OrgStatus,
  } from "../ipc";

  let s = $state<SettingsData | null>(null);
  let org = $state<OrgStatus | null>(null);
  // VSCode-style category tabs (left nav).
  const TABS = [
    { id: "general", label: "일반" },
    { id: "usage", label: "사용량 · 알림" },
    { id: "sync", label: "동기화" },
    { id: "org", label: "조직 리포팅" },
    { id: "about", label: "진단 · 정보" },
  ] as const;
  let tab = $state<(typeof TABS)[number]["id"]>("general");
  // Local inputs for the org section (committed on 등록).
  let orgUrl = $state("");
  let orgToken = $state("");
  let orgId = $state("");
  let orgMsg = $state("");
  let orgBusy = $state(false);

  const ORG_SOURCES = [
    { id: "claude_code", label: "Claude Code" },
    { id: "codex", label: "Codex" },
  ];
  let orgSources = $state<string[]>(["claude_code", "codex"]);

  function loadOrg() {
    getOrgStatus()
      .then((o) => {
        org = o;
        orgUrl = o.url ?? "";
        orgId = o.user_id ?? "";
        orgSources = o.sources;
      })
      .catch(() => {});
  }

  // At least one source must stay selected (an empty report is pointless).
  function toggleOrgSource(id: string, e: Event) {
    const on = (e.currentTarget as HTMLInputElement).checked;
    const next = on ? [...new Set([...orgSources, id])] : orgSources.filter((s) => s !== id);
    if (next.length === 0) {
      (e.currentTarget as HTMLInputElement).checked = true;
      return;
    }
    orgSources = next;
    void setOrgSources(next);
  }

  onMount(() => {
    getSettings().then((v) => (s = v));
    loadOrg();
  });

  async function registerOrg() {
    orgBusy = true;
    orgMsg = "";
    try {
      await setOrgConfig(
        org?.managed ? null : orgUrl,
        org?.managed ? null : orgToken || null,
        orgId,
      );
      await orgRegister();
      orgMsg = "등록 완료 — 이후 자동으로 전송됩니다";
      loadOrg();
    } catch (e) {
      orgMsg = `등록 실패: ${e}`;
    } finally {
      orgBusy = false;
    }
  }
  async function disableOrg() {
    await orgDisable();
    orgMsg = "";
    orgToken = "";
    loadOrg();
  }
  async function reportNow() {
    orgBusy = true;
    orgMsg = "";
    try {
      const rows = await orgReportNow();
      orgMsg = `전송 완료 — ${rows}행`;
      loadOrg();
    } catch (e) {
      orgMsg = `전송 실패: ${e}`;
    } finally {
      orgBusy = false;
    }
  }
  const fmtTime = (iso: string) => new Date(iso).toLocaleString();
  const intervalLabel = (secs: number) =>
    secs % 3600 === 0 ? `${secs / 3600}시간마다` : `${Math.round(secs / 60)}분마다`;
  const nextReport = (o: OrgStatus) =>
    o.last_report
      ? fmtTime(new Date(new Date(o.last_report).getTime() + o.interval_secs * 1000).toISOString())
      : "다음 새로고침 시";

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
  // "30, 50, 70, 90" → sanitized int list (backend re-normalizes too).
  function saveThresholds(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const parsed = input.value
      .split(/[,\s]+/)
      .map((t) => parseInt(t, 10))
      .filter((n) => Number.isFinite(n) && n >= 1 && n <= 100);
    const cleaned = [...new Set(parsed)].sort((a, b) => a - b);
    if (s) s.alert_thresholds = cleaned.length ? cleaned : [30, 50, 70, 90];
    input.value = (s?.alert_thresholds ?? cleaned).join(", ");
    void setAlertThresholds(cleaned);
  }
  function choosePercentDisplay(e: Event) {
    const mode = (e.currentTarget as HTMLSelectElement).value;
    if (s) s.percent_display = mode;
    void setPercentDisplay(mode);
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
  <aside class="nav">
    <h1>설정</h1>
    {#each TABS as t (t.id)}
      <button class="nav-item" class:active={tab === t.id} onclick={() => (tab = t.id)}>
        {t.label}
      </button>
    {/each}
    <div class="nav-foot muted">meterly v{s?.version ?? "…"}</div>
  </aside>

  <main class="content">
    {#if s === null}
      <p class="muted">불러오는 중…</p>
    {:else if tab === "general"}
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
        <h2>시작</h2>
        <label class="row-toggle">
          <input type="checkbox" checked={s.autostart} onchange={toggleAutostart} />
          로그인 시 자동 시작
        </label>
      </section>

      <section>
        <h2>날짜 표시 형식</h2>
        <label class="row-select">
          리셋 시각 등 날짜 표기
          <select value={s.date_format} onchange={chooseDateFormat}>
            {#each DATE_FORMATS as f}
              <option value={f.id}>{f.label}</option>
            {/each}
          </select>
        </label>
      </section>
    {:else if tab === "usage"}
      <section>
        <h2>한도 게이지</h2>
        <label class="row-select">
          표시 방식
          <select value={s.percent_display} onchange={choosePercentDisplay}>
            <option value="used">사용한 양</option>
            <option value="remaining">남은 양</option>
          </select>
        </label>
      </section>

      <section>
        <h2>알림</h2>
        <label class="row-toggle">
          <input type="checkbox" checked={s.alerts_enabled} onchange={toggleAlerts} />
          한도 사용률 알림 · 주간 리포트
        </label>
        <label class="row-select">
          알림 임계치 (%)
          <input
            class="thresholds"
            type="text"
            value={s.alert_thresholds.join(", ")}
            placeholder="30, 50, 70, 90"
            onchange={saveThresholds}
          />
        </label>
      </section>

      <section>
        <h2>월 토큰 예산</h2>
        <p class="muted small">
          이번 달 사용량과 월말 예상치를 대시보드에서 이 예산과 비교합니다. 비우면 예산
          없이 사용량·예상치만 표시합니다.
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
    {:else if tab === "sync"}
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
    {:else if tab === "org"}
      <section>
        <h2>조직 리포팅 <span class="muted">(선택)</span></h2>
        <p class="muted small">
          회사에서 사용량 수집을 운영하는 경우에만 설정하세요. 등록하면 일별 토큰
          사용량(날짜·도구·모델)만 주기적으로 전송됩니다 — 프롬프트·코드·프로젝트명은
          전송하지 않습니다.
        </p>
        {#if org?.managed}
          <div class="path">{org.url} <span class="muted small">(IT 관리 설정)</span></div>
        {:else}
          <input
            class="org-input"
            type="url"
            placeholder="수집 서버 URL (예: https://collect.example.com)"
            bind:value={orgUrl}
          />
          <input
            class="org-input"
            type="password"
            placeholder="토큰 (선택)"
            bind:value={orgToken}
          />
        {/if}
        <input
          class="org-input"
          type="text"
          placeholder="식별자 (사번 등, 조직 가이드에 따라 입력)"
          bind:value={orgId}
        />
        <div class="src-row">
          <span class="muted small">전송할 도구</span>
          {#each ORG_SOURCES as src (src.id)}
            <label class="row-toggle small">
              <input
                type="checkbox"
                checked={orgSources.includes(src.id)}
                onchange={(e) => toggleOrgSource(src.id, e)}
              />
              {src.label}
            </label>
          {/each}
        </div>
        <div class="btn-row">
          <button onclick={registerOrg} disabled={orgBusy || !orgId || (!org?.managed && !orgUrl)}>
            {orgBusy ? "처리 중…" : org?.registered ? "다시 등록" : "등록"}
          </button>
          {#if org?.registered}
            <button onclick={reportNow} disabled={orgBusy}>지금 전송</button>
          {/if}
          {#if org?.registered || org?.url}
            <button class="ghost" onclick={disableOrg}>해제</button>
          {/if}
        </div>
        {#if org?.last_error}
          <div class="org-error" role="alert">
            <b>⚠ 등록되지 않은 식별자</b>
            <p>{org.last_error}</p>
            <p class="hint">
              위 안내대로 <b>정확한 이메일(사번)</b>을 식별자 칸에 입력하고 다시
              [등록]을 눌러주세요. 서버에 등록된 값과 한 글자라도 다르면 거부됩니다.
            </p>
          </div>
        {/if}
        {#if orgMsg}
          <p class="small" class:err={orgMsg.includes("실패")}>{orgMsg}</p>
        {/if}
      </section>

      {#if org?.registered}
        <section>
          <h2>리포팅 상태</h2>
          <div class="org-status">
            <div class="st-row">
              <span class="st-key">상태</span>
              <span><span class="st-ok">● 등록됨</span> — {org.user_id} @ {org.hostname}</span>
            </div>
            <div class="st-row">
              <span class="st-key">전송 주기</span>
              <span>{intervalLabel(org.interval_secs)} (실패 시 다음 새로고침에 재시도)</span>
            </div>
            <div class="st-row">
              <span class="st-key">마지막 전송</span>
              <span>{org.last_report ? fmtTime(org.last_report) : "아직 없음 (첫 전송 대기)"}</span>
            </div>
            <div class="st-row">
              <span class="st-key">다음 전송</span>
              <span>{nextReport(org)}</span>
            </div>
            <div class="st-row">
              <span class="st-key">전송 도구</span>
              <span>
                {ORG_SOURCES.filter((x) => orgSources.includes(x.id))
                  .map((x) => x.label)
                  .join(" · ")}
              </span>
            </div>
            <div class="st-row">
              <span class="st-key">전송 대상</span>
              <span class="mono">{org.url}</span>
            </div>
          </div>
        </section>
      {:else if org?.url || org?.managed}
        <section>
          <h2>리포팅 상태</h2>
          {#if org?.last_error}
            <p class="muted small">
              ● 등록 거부됨 — 식별자가 서버에 없습니다. 위 안내를 확인해 정확한
              이메일(사번)로 다시 등록하세요.
            </p>
          {:else}
            <p class="muted small">
              ● 미등록 — 식별자를 입력하고 [등록]을 눌러야 전송이 시작됩니다.
            </p>
          {/if}
        </section>
      {/if}
    {:else if tab === "about"}
      <section>
        <h2>진단</h2>
        <p class="muted small">
          문제가 있을 때 참고할 로그를 이 기기에 일 단위로 최대 7일 보관합니다.
        </p>
        <div class="btn-row">
          <button onclick={() => openLogDir()}>로그 폴더 열기</button>
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
  </main>
</div>

<style>
  /* VSCode-style: fixed left category nav + scrolling content pane. */
  .settings {
    display: flex;
    height: 100%;
    box-sizing: border-box;
    font-size: 13px;
  }
  .nav {
    flex: 0 0 168px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 1rem 0.6rem;
    box-sizing: border-box;
    border-right: 1px solid color-mix(in srgb, CanvasText 14%, transparent);
    background: color-mix(in srgb, CanvasText 3%, transparent);
  }
  h1 {
    margin: 0 0 0.7rem;
    padding: 0 0.5rem;
    font-size: 1.05rem;
  }
  .nav-item {
    font: inherit;
    font-size: 0.85rem;
    text-align: left;
    padding: 0.42rem 0.6rem;
    border: none;
    border-radius: 7px;
    background: transparent;
    color: color-mix(in srgb, CanvasText 75%, transparent);
    cursor: pointer;
  }
  .nav-item:hover {
    background: color-mix(in srgb, CanvasText 8%, transparent);
  }
  .nav-item.active {
    background: color-mix(in srgb, #4f8ef7 22%, transparent);
    color: CanvasText;
    font-weight: 600;
  }
  .nav-foot {
    margin-top: auto;
    padding: 0 0.5rem;
    font-size: 0.72rem;
  }
  .content {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1.3rem;
    padding: 1.2rem 1.4rem 2rem;
    box-sizing: border-box;
    overflow-y: auto;
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
  .thresholds {
    width: 9rem;
    font: inherit;
    font-size: 0.85rem;
    padding: 0.25rem 0.5rem;
    border-radius: 7px;
    border: 1px solid color-mix(in srgb, CanvasText 25%, transparent);
    background: color-mix(in srgb, CanvasText 6%, transparent);
    color: inherit;
    text-align: right;
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
  .org-input {
    font: inherit;
    font-size: 0.85rem;
    padding: 0.35rem 0.5rem;
    border-radius: 7px;
    border: 1px solid color-mix(in srgb, CanvasText 25%, transparent);
    background: color-mix(in srgb, CanvasText 6%, transparent);
    color: inherit;
    width: 100%;
    box-sizing: border-box;
  }
  .err {
    color: #e0524f;
  }
  .org-error {
    margin-top: 0.6rem;
    padding: 0.6rem 0.75rem;
    border: 1px solid color-mix(in srgb, #e0524f 55%, transparent);
    border-radius: 8px;
    background: color-mix(in srgb, #e0524f 12%, transparent);
    font-size: 0.85rem;
  }
  .org-error b {
    color: #e0524f;
  }
  .org-error p {
    margin: 0.35rem 0 0;
  }
  .org-error .hint {
    color: color-mix(in srgb, CanvasText 65%, transparent);
    font-size: 0.8rem;
  }
  .src-row {
    display: flex;
    align-items: center;
    gap: 1rem;
  }
  /* Org reporting status panel. */
  .org-status {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.6rem 0.75rem;
    border: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    border-radius: 8px;
    background: color-mix(in srgb, CanvasText 4%, transparent);
    font-size: 0.82rem;
  }
  .st-row {
    display: flex;
    gap: 0.75rem;
  }
  .st-key {
    flex: 0 0 5.5rem;
    color: color-mix(in srgb, CanvasText 55%, transparent);
  }
  .st-ok {
    color: #2fa653;
    font-weight: 600;
  }
  .mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.78rem;
    word-break: break-all;
  }
</style>
