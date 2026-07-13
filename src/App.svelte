<script lang="ts">
  // Window routing: popover vs dashboard, selected via URL query
  // (tauri.conf.json window `url` fields).
  import PopoverSummary from "./lib/popover/PopoverSummary.svelte";
  import Dashboard from "./lib/dashboard/Dashboard.svelte";

  const params = new URLSearchParams(window.location.search);
  const windowKind = params.get("window") === "dashboard" ? "dashboard" : "popover";
</script>

{#if windowKind === "dashboard"}
  <Dashboard />
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
