<script lang="ts">
  // Tiny inline-SVG sparkline (7-day totals). Single series → no legend;
  // the surrounding row names it. Gradient-faded area + 2px line so it reads
  // as a soft trend band rather than a chart.
  let {
    values,
    color,
    width = 72,
    height = 22,
  }: { values: number[]; color: string; width?: number; height?: number } =
    $props();

  const PAD = 2;
  // Gradient ids must be unique per instance or every sparkline on the page
  // would share the first one's colour.
  const gid = `spark-${Math.random().toString(36).slice(2, 9)}`;

  function pointsOf(vals: number[]): { line: string; area: string; last: string } {
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
    return { line, area, last: pts[pts.length - 1] ?? "" };
  }

  const shape = $derived(pointsOf(values));
  const empty = $derived(values.every((v) => v === 0));
  const last = $derived(shape.last.split(",").map(Number));
</script>

{#if !empty}
  <svg
    {width}
    {height}
    viewBox={`0 0 ${width} ${height}`}
    role="img"
    aria-label="최근 7일 추이"
    overflow="visible"
  >
    <defs>
      <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color={color} stop-opacity="0.45" />
        <stop offset="1" stop-color={color} stop-opacity="0" />
      </linearGradient>
    </defs>
    <polygon points={shape.area} fill={`url(#${gid})`} />
    <polyline
      points={shape.line}
      fill="none"
      stroke={color}
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
    <!-- End-point dot: "today" — the value the hero number is showing. -->
    <circle cx={last[0]} cy={last[1]} r="2.6" fill={color} />
  </svg>
{/if}
