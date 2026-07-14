<script lang="ts">
  // Window routing: popover vs dashboard, selected via URL query
  // (tauri.conf.json window `url` fields).
  import PopoverSummary from "./lib/popover/PopoverSummary.svelte";
  import Dashboard from "./lib/dashboard/Dashboard.svelte";
  import Settings from "./lib/settings/Settings.svelte";
  import { openSettings } from "./lib/ipc";

  const params = new URLSearchParams(window.location.search);
  const q = params.get("window");
  const windowKind =
    q === "dashboard" ? "dashboard" : q === "settings" ? "settings" : "popover";

  // Cmd+, opens Settings while any meterly window is focused (standard macOS
  // Preferences shortcut; the app is menu-bar-only so this is per-window).
  function onKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === ",") {
      e.preventDefault();
      openSettings();
    }
  }
</script>

<svelte:window on:keydown={onKey} />

{#if windowKind === "dashboard"}
  <Dashboard />
{:else if windowKind === "settings"}
  <Settings />
{:else}
  <PopoverSummary />
{/if}

<style>
  :global(html, body, #app) {
    margin: 0;
    height: 100%;
    font-family:
      -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    font-size: 14px;
  }
  :global(body) {
    background: Canvas;
    color: CanvasText;
    color-scheme: light dark;
  }
</style>
