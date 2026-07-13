<script lang="ts">
  // Tiny inline-SVG sparkline (7-day totals). Single series → no legend;
  // the surrounding row names it. Area + 2px line per the mark spec.
  let {
    values,
    color,
    width = 72,
    height = 22,
  }: { values: number[]; color: string; width?: number; height?: number } =
    $props();

  const PAD = 2;

  function pointsOf(vals: number[]): { line: string; area: string } {
    const max = Math.max(...vals, 1);
    const n = vals.length;
    const step = (width - PAD * 2) / Math.max(n - 1, 1);
    const pts = vals.map((v, i) => {
      const x = PAD + i * step;
      const y = height - PAD - (v / max) * (height - PAD * 2);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    });
    const line = pts.join(" ");
    const area = `${PAD},${height - PAD} ${line} ${(PAD + (n - 1) * step).toFixed(1)},${height - PAD}`;
    return { line, area };
  }

  const shape = $derived(pointsOf(values));
  const empty = $derived(values.every((v) => v === 0));
</script>

{#if !empty}
  <svg
    {width}
    {height}
    viewBox={`0 0 ${width} ${height}`}
    role="img"
    aria-label="최근 7일 추이"
  >
    <polygon points={shape.area} fill={color} opacity="0.15" />
    <polyline
      points={shape.line}
      fill="none"
      stroke={color}
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
  </svg>
{/if}
