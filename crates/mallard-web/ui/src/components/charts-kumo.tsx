// Line charts: Kumo's TimeseriesChart on a tree-shaken ECharts, in Kumo's palette.
import { ChartPalette, TimeseriesChart } from "@cloudflare/kumo/components/chart";
import { BarChart, LineChart } from "echarts/charts";
import { AriaComponent, AxisPointerComponent, BrushComponent, GridComponent, LegendComponent, MarkLineComponent, ToolboxComponent, TooltipComponent } from "echarts/components";
import * as echarts from "echarts/core";
import { CanvasRenderer } from "echarts/renderers";
import { useSettings } from "@/app/settings";
import { Legend } from "@/components/charts";
import { axisTime, timestamp } from "@/lib/format";

echarts.use([LineChart, BarChart, GridComponent, TooltipComponent, AxisPointerComponent, BrushComponent, LegendComponent, MarkLineComponent, ToolboxComponent, AriaComponent, CanvasRenderer]);

export interface Line {
  name: string;
  /** Index into the categorical palette; follows the entity, never its rank. */
  slot: number;
  /** A failure line uses the reserved colour instead. */
  attention?: boolean;
  points: [number, number][];
}

export function lineColor(l: Pick<Line, "slot" | "attention">, isDark: boolean): string {
  return l.attention ? ChartPalette.semantic("Attention", isDark) : ChartPalette.categorical(l.slot, isDark);
}

/** Identity is never colour alone: every chart with several lines names them. */
export function ChartLegend({ lines }: { lines: Line[] }) {
  const { isDark } = useSettings();
  return <Legend items={lines.map((l) => ({ label: l.name, color: lineColor(l, isDark) }))} />;
}

export function TimeChart({ lines, height = 220, format, spanMs, type = "line", minInterval }: {
  lines: Line[];
  height?: number;
  format: (v: number) => string;
  spanMs: number;
  type?: "line" | "bar";
  minInterval?: number;
}) {
  const { isDark } = useSettings();
  return (
    <TimeseriesChart
      echarts={echarts}
      isDarkMode={isDark}
      type={type}
      height={height}
      data={lines.map((l) => ({ name: l.name, color: lineColor(l, isDark), data: l.points }))}
      xAxisTickFormat={(t) => axisTime(t, spanMs)}
      yAxisTickFormat={format}
      tooltipValueFormat={format}
      tooltipTimestampFormat={(t) => `${timestamp(new Date(t))} UTC`}
      yAxisMinInterval={minInterval}
    />
  );
}
