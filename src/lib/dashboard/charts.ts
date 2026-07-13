// Chart.js rendering (T10 + viz polish). Bundled locally — no CDN.
//
// Colors come from the dataviz reference palette, VALIDATED per mode with
// scripts/validate_palette.js (2026-07-14): source pair ΔE 96.7/97.3 PASS;
// token-category quad PASS (light aqua/yellow < 3:1 → relief via legend +
// 2px surface gaps; dark adjacent ΔE 10.3 floor band → same gap relief).
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

// ---- theme (light/dark selected, not flipped) ----

export interface Theme {
  surface: string;
  text: string;
  grid: string;
  sources: Record<SourceId, string>;
  categories: [string, string, string, string]; // input/output/cache_read/cache_creation
  modelSlots: string[]; // fixed categorical order (validated set)
  other: string;
  /** Sequential blue ramp, near-zero → max (heatmap magnitude). */
  seq: string[];
}

const LIGHT: Theme = {
  surface: "#fcfcfb",
  text: "#52514e",
  grid: "rgba(82, 81, 78, 0.14)",
  sources: { claude_code: "#eb6834", codex: "#2a78d6" },
  categories: ["#2a78d6", "#1baf7a", "#eda100", "#008300"],
  modelSlots: [
    "#2a78d6", "#1baf7a", "#eda100", "#008300",
    "#4a3aa7", "#e34948", "#e87ba4", "#eb6834",
  ],
  other: "#8a8983",
  seq: ["#e9f1fb", "#b7d3f6", "#86b6ef", "#5598e7", "#2a78d6", "#1c5cab"],
};

const DARK: Theme = {
  surface: "#1a1a19",
  text: "#c3c2b7",
  grid: "rgba(195, 194, 183, 0.16)",
  sources: { claude_code: "#d95926", codex: "#3987e5" },
  categories: ["#3987e5", "#199e70", "#c98500", "#008300"],
  modelSlots: [
    "#3987e5", "#199e70", "#c98500", "#008300",
    "#9085e9", "#e66767", "#d55181", "#d95926",
  ],
  other: "#8a8983",
  seq: ["#22293a", "#184f95", "#256abf", "#3987e5", "#6da7ec", "#9ec5f4"],
};

export function theme(): Theme {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? DARK : LIGHT;
}

export const SOURCE_LABELS: Record<SourceId, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
};

const charts = new WeakMap<HTMLCanvasElement, Chart>();

function replaceChart(canvas: HTMLCanvasElement, config: any): void {
  charts.get(canvas)?.destroy();
  charts.set(canvas, new Chart(canvas, config));
}

/** Shared look: recessive grid/axes, thin marks, legend as small chips. */
function baseOptions(t: Theme, opts: { horizontal?: boolean; money?: boolean } = {}) {
  const fmt = (v: number) => (opts.money ? `$${v}` : formatTokens(v));
  const valueAxis = {
    stacked: true,
    grid: { color: t.grid },
    border: { display: false },
    ticks: { color: t.text, font: { size: 10 }, callback: (v: any) => fmt(Number(v)) },
  };
  const labelAxis = {
    stacked: true,
    grid: { display: false },
    border: { display: false },
    ticks: { color: t.text, font: { size: 10 }, maxRotation: 0, autoSkip: true },
  };
  return {
    responsive: true,
    maintainAspectRatio: false,
    indexAxis: opts.horizontal ? ("y" as const) : ("x" as const),
    scales: opts.horizontal
      ? { x: valueAxis, y: labelAxis }
      : { x: labelAxis, y: valueAxis },
    plugins: {
      legend: {
        position: "top" as const,
        align: "end" as const,
        labels: {
          color: t.text,
          usePointStyle: true,
          pointStyle: "rectRounded" as const,
          boxWidth: 8,
          boxHeight: 8,
          font: { size: 10 },
        },
      },
      tooltip: {
        backgroundColor: t.surface,
        titleColor: t.text,
        bodyColor: t.text,
        borderColor: t.grid,
        borderWidth: 1,
        callbacks: {
          label: (ctx: any) => {
            const v = opts.horizontal ? ctx.parsed.x : ctx.parsed.y;
            return `${ctx.dataset.label}: ${
              opts.money ? `$${Number(v).toFixed(2)}` : `${formatTokens(v)} tok`
            }`;
          },
        },
      },
    },
  };
}

/** Mark spec: rounded segment ends + 2px surface gap between fills. */
function barStyle(t: Theme, color: string) {
  return {
    backgroundColor: color,
    borderColor: t.surface,
    borderWidth: 1,
    borderRadius: 3,
    borderSkipped: false as const,
    maxBarThickness: 26,
  };
}

function periodsOf(data: DashboardData): string[] {
  return [...new Set(data.rows.map((r) => r.period))].sort();
}

/** 추이: stacked bars per period, one dataset per source. */
export function renderTrendChart(canvas: HTMLCanvasElement, data: DashboardData): void {
  const t = theme();
  const periods = periodsOf(data);
  const sources = [...new Set(data.rows.map((r) => r.source))].sort() as SourceId[];
  const datasets = sources.map((src) => ({
    label: SOURCE_LABELS[src] ?? src,
    ...barStyle(t, t.sources[src] ?? t.other),
    data: periods.map((p) =>
      data.rows
        .filter((r) => r.period === p && r.source === src)
        .reduce((sum, r) => sum + r.tokens.total, 0),
    ),
  }));
  replaceChart(canvas, {
    type: "bar",
    data: { labels: periods, datasets },
    options: baseOptions(t),
  });
}

/** 토큰 구성: stacked bars per period, one dataset per token category. */
export function renderCompositionChart(
  canvas: HTMLCanvasElement,
  data: DashboardData,
): void {
  const t = theme();
  const periods = periodsOf(data);
  const cats = [
    { key: "input", label: "입력" },
    { key: "output", label: "출력" },
    { key: "cache_read", label: "캐시 읽기" },
    { key: "cache_creation", label: "캐시 생성" },
  ] as const;
  const datasets = cats.map((c, i) => ({
    label: c.label,
    ...barStyle(t, t.categories[i]),
    data: periods.map((p) =>
      data.rows
        .filter((r) => r.period === p)
        .reduce((sum, r) => sum + r.tokens[c.key], 0),
    ),
  }));
  replaceChart(canvas, {
    type: "bar",
    data: { labels: periods, datasets },
    options: baseOptions(t),
  });
}

/** 비용 추이: stacked bars per period per source (known models only). */
export function renderCostChart(canvas: HTMLCanvasElement, data: DashboardData): void {
  const t = theme();
  const periods = periodsOf(data);
  const sources = [...new Set(data.rows.map((r) => r.source))].sort() as SourceId[];
  const datasets = sources.map((src) => ({
    label: SOURCE_LABELS[src] ?? src,
    ...barStyle(t, t.sources[src] ?? t.other),
    data: periods.map((p) =>
      Number(
        data.rows
          .filter((r) => r.period === p && r.source === src)
          .reduce((sum, r) => sum + (r.cost_usd ?? 0), 0)
          .toFixed(2),
      ),
    ),
  }));
  replaceChart(canvas, {
    type: "bar",
    data: { labels: periods, datasets },
    options: baseOptions(t, { money: true }),
  });
}

/** 모델별 비교: horizontal bars, top 8 + 기타. Hues follow the MODEL
 * (stable alphabetical slot assignment), never its rank. */
export function renderModelChart(canvas: HTMLCanvasElement, data: DashboardData): void {
  const t = theme();
  const byModel = new Map<string, number>();
  for (const r of data.rows) {
    const key = r.model ?? "unknown";
    byModel.set(key, (byModel.get(key) ?? 0) + r.tokens.total);
  }
  const ranked = [...byModel.entries()].sort((a, b) => b[1] - a[1]);
  const top = ranked.slice(0, 8);
  const restTotal = ranked.slice(8).reduce((s, [, v]) => s + v, 0);

  // Stable identity: hue slot by alphabetical position among the shown set.
  const alpha = top.map(([m]) => m).sort();
  const colorOf = (model: string) =>
    t.modelSlots[alpha.indexOf(model) % t.modelSlots.length];

  const labels = top.map(([m]) => m);
  const colors = top.map(([m]) => colorOf(m));
  const values = top.map(([, v]) => v);
  if (restTotal > 0) {
    labels.push("기타");
    colors.push(t.other);
    values.push(restTotal);
  }
  const options = baseOptions(t, { horizontal: true });
  (options.plugins as any).legend = { display: false };
  replaceChart(canvas, {
    type: "bar",
    data: {
      labels,
      datasets: [
        {
          label: "tokens",
          data: values,
          backgroundColor: colors,
          borderColor: t.surface,
          borderWidth: 1,
          borderRadius: 3,
          borderSkipped: false,
          maxBarThickness: 18,
        },
      ],
    },
    options,
  });
}
