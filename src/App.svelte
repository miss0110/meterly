<script lang="ts">
  // Window routing: popover vs dashboard, selected via URL query
  // (tauri.conf.json window `url` fields). Real UI arrives in T9/T10.
  const params = new URLSearchParams(window.location.search);
  const windowKind = params.get("window") === "dashboard" ? "dashboard" : "popover";
</script>

{#if windowKind === "dashboard"}
  <main class="dashboard">
    <h1>meterly — dashboard</h1>
    <p>Dashboard UI arrives in T10.</p>
  </main>
{:else}
  <main class="popover">
    <h1>meterly</h1>
    <p>Popover UI arrives in T9.</p>
  </main>
{/if}

<style>
  main {
    font-family:
      -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    padding: 1rem;
  }
  h1 {
    font-size: 1rem;
    margin: 0 0 0.5rem;
  }
  p {
    font-size: 0.85rem;
    color: #666;
    margin: 0;
  }
</style>
