// Chart.js rendering for the dashboard (T10). Bundled locally — no CDN.
import {
  Chart,
  BarController,
  BarElement,
  CategoryScale,
  LinearScale,
  Legend,
  Tooltip,
} from "chart.js";
import type { DashboardData, SourceId } from "../ipc";
import { formatTokens } from "../format";

Chart.register(BarController, BarElement, CategoryScale, LinearScale, Legend, Tooltip);

const SOURCE_COLORS: Record<SourceId, string> = {
  claude_code: "#d97757",
  codex: "#4f8ef7",
};
const SOURCE_LABELS: Record<SourceId, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
};
const MODEL_PALETTE = [
  "#4f8ef7",
  "#d97757",
  "#41b883",
  "#b57edc",
  "#e8b339",
  "#e06c75",
  "#56b6c2",
];

const charts = new WeakMap<HTMLCanvasElement, Chart>();

function replaceChart(canvas: HTMLCanvasElement, config: any): void {
  charts.get(canvas)?.destroy();
  charts.set(canvas, new Chart(canvas, config));
}

const axisTicks = {
  callback: (value: number | string) => formatTokens(Number(value)),
};

/** Stacked bars per period, one dataset per source. */
export function renderTrendChart(canvas: HTMLCanvasElement, data: DashboardData): void {
  const periods = [...new Set(data.rows.map((r) => r.period))].sort();
  const sources = [...new Set(data.rows.map((r) => r.source))];
  const datasets = sources.map((src) => ({
    label: SOURCE_LABELS[src] ?? src,
    backgroundColor: SOURCE_COLORS[src] ?? "#999",
    borderRadius: 3,
    data: periods.map((p) =>
      data.rows
        .filter((r) => r.period === p && r.source === src)
        .reduce((sum, r) => sum + r.tokens.total, 0),
    ),
  }));
  replaceChart(canvas, {
    type: "bar",
    data: { labels: periods, datasets },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      scales: {
        x: { stacked: true, grid: { display: false } },
        y: { stacked: true, ticks: axisTicks },
      },
      plugins: {
        tooltip: {
          callbacks: {
            label: (ctx: any) =>
              `${ctx.dataset.label}: ${formatTokens(ctx.parsed.y)} tok`,
          },
        },
      },
    },
  });
}

/** Horizontal bars: token total per model over the whole range. */
export function renderModelChart(canvas: HTMLCanvasElement, data: DashboardData): void {
  const byModel = new Map<string, number>();
  for (const r of data.rows) {
    const key = r.model ?? "unknown";
    byModel.set(key, (byModel.get(key) ?? 0) + r.tokens.total);
  }
  const entries = [...byModel.entries()].sort((a, b) => b[1] - a[1]).slice(0, 8);
  replaceChart(canvas, {
    type: "bar",
    data: {
      labels: entries.map(([model]) => model),
      datasets: [
        {
          label: "tokens",
          data: entries.map(([, total]) => total),
          backgroundColor: entries.map((_, i) => MODEL_PALETTE[i % MODEL_PALETTE.length]),
          borderRadius: 3,
        },
      ],
    },
    options: {
      indexAxis: "y",
      responsive: true,
      maintainAspectRatio: false,
      scales: { x: { ticks: axisTicks } },
      plugins: {
        legend: { display: false },
        tooltip: {
          callbacks: {
            label: (ctx: any) => `${formatTokens(ctx.parsed.x)} tok`,
          },
        },
      },
    },
  });
}
