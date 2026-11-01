// Marks drawn as SVG or divs (sparklines, stacked bars); line charts come from
// Kumo's TimeseriesChart in charts-kumo.tsx. Thin marks, one axis, hover values.
import { useId, useMemo, useState } from "react";
import { compact } from "@/lib/format";
import { resample } from "@/lib/domain";
import { cn } from "@/lib/utils";

/** Trend in a row: no axes, the exact value sits beside it. */
export function Sparkline({ values, width = 96, height = 22, label, className, fluid }: { values: number[]; width?: number; height?: number; label: string; className?: string; fluid?: boolean }) {
  const bars = useMemo(() => resample(values, 24), [values]);
  const max = Math.max(...bars, 0);
  const step = width / Math.max(bars.length, 1);
  const total = values.reduce((a, b) => a + b, 0);
  return (
    <svg role="img" aria-label={`${label}: ${compact(total)} in total`} width={fluid ? "100%" : width} height={height} viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" className={cn("block text-(--color-kumo-brand)", className)}>
      <title>{`${label}: ${compact(total)} in total, peak ${compact(Math.max(...values, 0))}`}</title>
      {bars.map((v, i) => {
        const h = v > 0 ? Math.max(2, (v / max) * (height - 1)) : 1;
        return (
          <rect
            key={i}
            x={i * step}
            y={height - h}
            width={Math.max(step - 1, 1)}
            height={h}
            rx={v > 0 ? 1 : 0}
            fill="currentColor"
            opacity={v > 0 ? 1 : 0.18}
          />
        );
      })}
    </svg>
  );
}

export interface StackSeries {
  key: string;
  label: string;
  /** CSS colour. */
  color: string;
}

export interface StackBucket {
  time: number;
  values: Record<string, number>;
}

/** Stacked bars over time with a per-bucket tooltip. */
export function StackedBars({ buckets, series, height = 72, format = compact, axis, ariaLabel, stepLabel }: {
  buckets: StackBucket[];
  series: StackSeries[];
  height?: number;
  format?: (v: number) => string;
  axis?: [string, string];
  ariaLabel: string;
  stepLabel?: (time: number) => string;
}) {
  const [hover, setHover] = useState<{ index: number; x: number; y: number } | null>(null);
  const titleId = useId();
  const totals = buckets.map((b) => series.reduce((a, s) => a + (b.values[s.key] ?? 0), 0));
  const max = Math.max(...totals, 0);
  const hovered = hover === null ? null : buckets[hover.index];
  return (
    <figure className="relative m-0" aria-labelledby={titleId}>
      <figcaption id={titleId} className="sr-only">{ariaLabel}</figcaption>
      <div className="flex items-end gap-px" style={{ height }} role="img" aria-label={ariaLabel} onMouseLeave={() => setHover(null)}>
        {buckets.map((b, i) => (
          <div key={b.time} className="relative flex h-full min-w-0 flex-1 flex-col justify-end" onMouseEnter={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setHover({ index: i, x: r.left + r.width / 2, y: r.top });
          }}>
            {hover?.index === i && <div className="absolute inset-0 rounded-sm bg-kumo-tint" aria-hidden />}
            <div className="relative flex flex-col-reverse gap-px">
              {series.map((s) => {
                const v = b.values[s.key] ?? 0;
                if (v <= 0 || max <= 0) return null;
                return <div key={s.key} className="min-h-px rounded-[1px]" style={{ height: Math.max(1, (v / max) * (height - 2)), background: s.color }} />;
              })}
            </div>
          </div>
        ))}
      </div>
      {axis && (
        <div className="tnum mt-1 flex justify-between text-xs text-kumo-subtle">
          <span>{axis[0]}</span>
          <span>{axis[1]}</span>
        </div>
      )}
      {hovered && (
        <div
          className="tnum pointer-events-none fixed z-50 min-w-32 rounded-md bg-kumo-overlay px-2 py-1.5 text-xs shadow-md ring ring-kumo-line"
          style={{
            top: hover!.y - 6,
            left: Math.max(8, Math.min(hover!.x, window.innerWidth - 8)),
            transform: `translate(${hover!.x > window.innerWidth - 200 ? "-100%" : hover!.x < 200 ? "0" : "-50%"}, -100%)`,
          }}
        >
          {stepLabel && <div className="mb-1 text-kumo-subtle">{stepLabel(hovered.time)}</div>}
          {series.map((s) => (
            <div key={s.key} className="flex items-center justify-between gap-4">
              <span className="flex items-center gap-1.5"><span aria-hidden className="size-2 rounded-[2px]" style={{ background: s.color }} />{s.label}</span>
              <span>{format(hovered.values[s.key] ?? 0)}</span>
            </div>
          ))}
        </div>
      )}
    </figure>
  );
}

export function Legend({ items }: { items: { label: string; color: string; value?: string }[] }) {
  return (
    <ul className="tnum flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-kumo-subtle">
      {items.map((i) => (
        <li key={i.label} className="flex items-center gap-1.5">
          <span aria-hidden className="size-2 rounded-[2px]" style={{ background: i.color }} />
          {i.label}
          {i.value && <span className="text-kumo-default">{i.value}</span>}
        </li>
      ))}
    </ul>
  );
}
