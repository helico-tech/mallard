import { useEffect, useMemo, useRef, useState } from "react";
import { Button, Input, Table, Tabs } from "@cloudflare/kumo";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useParams } from "react-router-dom";
import { otel } from "@/api/otel";
import { useKeys } from "@/app/keys";
import { useInterval, useSettings } from "@/app/settings";
import { useParam, useScope, useWindow } from "@/app/window";
import { ChartLegend, lineColor, TimeChart, type Line } from "@/components/charts-kumo";
import { useBack, DataTable, DetailHeader, EmptyState, ErrorState, Pad, Page, Section, StaleNote, TableSkeleton } from "@/components/ui-bits";
import { clean } from "@/lib/clean";
import { SERIES_COUNT, windowSeconds } from "@/lib/domain";
import { aggregations, metricTypeLabel, metricValue, seriesLabel } from "@/lib/metrics";

export default function MetricPage() {
  const { name = "" } = useParams();
  const { w } = useWindow();
  const { services } = useScope();
  const [aggParam, setAgg] = useParam("agg");
  const [by, setBy] = useParam("by");
  const [draft, setDraft] = useState(by);
  const byRef = useRef<HTMLInputElement>(null);
  const refetchInterval = useInterval(10000);
  const { isDark } = useSettings();
  useEffect(() => setDraft(by), [by]);

  const info = useQuery({ queryKey: ["otel", "metrics", w, services], queryFn: ({ signal }) => otel.metrics(w, services ?? undefined, signal), placeholderData: keepPreviousData });
  const meta = info.data?.find((m) => m.name === name);
  const aggs = meta ? aggregations(meta.metric_type, meta.temporality) : [];
  const agg = aggParam && aggs.includes(aggParam) ? aggParam : aggs[0];

  const q = useQuery({
    queryKey: ["otel", "metric", name, w, agg, by, services],
    enabled: !!agg,
    queryFn: ({ signal }) => otel.metric({ name, since: w, agg, group_by: by || undefined, service: services ?? undefined }, signal),
    refetchInterval,
  });

  const back = useBack(`/telemetry/metrics?w=${w}`);
  const step = (dir: 1 | -1) => {
    if (!aggs.length) return;
    const i = aggs.indexOf(agg ?? "");
    setAgg(aggs[(i + dir + aggs.length) % aggs.length]!, true);
  };
  useKeys({
    a: () => step(1),
    ArrowRight: () => step(1),
    A: () => step(-1),
    b: () => byRef.current?.focus(),
    Escape: back,
    Backspace: back,
    h: back,
    q: back,
    ArrowLeft: back,
  });

  const result = q.data;
  const unit = result?.unit ?? meta?.unit ?? "";
  const shown = useMemo(() => (result?.series ?? []).slice(0, SERIES_COUNT), [result]);
  const hidden = (result?.series.length ?? 0) - shown.length;
  const maxAll = useMemo(() => Math.max(0, ...shown.flatMap((s) => s.points.map((p) => p.value))), [shown]);
  const format = (v: number) => metricValue(v, agg ?? "", unit, maxAll);
  const lines: Line[] = useMemo(
    () => shown.map((s, i) => ({ name: seriesLabel(s.labels, agg ?? "").slice(0, 60), slot: i, points: s.points.map((p): [number, number] => [Date.parse(p.time), p.value]) })),
    [shown, agg],
  );

  return (
    <Page scroll>
      <DetailHeader
        title={<span className="mono">{clean(name)}</span>}
        meta={meta && (
          <>
            <span>{metricTypeLabel(meta.metric_type)}{meta.unit ? `, ${meta.unit}` : ""}</span>
            {meta.description && <span>{meta.description}</span>}
          </>
        )}
      />
      <div className="flex shrink-0 flex-wrap items-center gap-3">
        {aggs.length > 0 && (
          <Tabs  aria-label="Aggregation" value={agg} onValueChange={(v) => setAgg(v)} tabs={aggs.map((a) => ({ value: a, label: /^p\d/.test(a) ? a : a[0]!.toUpperCase() + a.slice(1) }))} />
        )}
        <form
          className="flex items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setBy(draft.trim());
          }}
        >
          <span className="text-kumo-subtle">Group by</span>
          <Input
            ref={byRef}
            aria-label="Group by attribute keys, comma separated"
            className="mono w-64"
            placeholder="http.route,service.name"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setDraft(by);
                e.currentTarget.blur();
              }
            }}
          />
          {by && <Button variant="ghost" onClick={() => setBy("")}>Clear</Button>}
        </form>
      </div>
      {q.isError && q.data && <StaleNote error={q.error} />}
      {q.isError && !q.data ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} title="Could not load this metric" />
      ) : !result ? (
        info.data && !meta ? <EmptyState title={`No metric "${clean(name)}" in the last ${w}`} description="Widen the time window." /> : <TableSkeleton rows={6} />
      ) : shown.every((s) => s.points.length === 0) ? (
        <EmptyState title={`No data in the last ${w}`} description="Widen the time window." />
      ) : (
        <>
          <Section title="Over time" right={lines.length > 1 ? <ChartLegend lines={lines} /> : undefined}><Pad><TimeChart lines={lines} height={320} format={format} spanMs={windowSeconds(w) * 1000} /></Pad></Section>
          <Section title="Series" right={hidden > 0 ? `showing ${shown.length} of ${result.series.length}` : `${shown.length} series`}>
            <DataTable>
              <caption className="sr-only">Summary of each series</caption>
              <Table.Header variant="compact" sticky><Table.Row><Table.Head className="w-6"><span className="sr-only">Colour</span></Table.Head><Table.Head>Series</Table.Head><Table.Head className="text-right">Last</Table.Head><Table.Head className="text-right">Min</Table.Head><Table.Head className="text-right">Avg</Table.Head><Table.Head className="text-right">Max</Table.Head></Table.Row></Table.Header>
              <Table.Body>
                {shown.map((s, i) => {
                  const v = s.points.map((p) => p.value);
                  const last = v[v.length - 1] ?? 0;
                  return (
                    <Table.Row key={i}>
                      <Table.Cell><span aria-hidden className="block h-0.5 w-4 rounded-full" style={{ background: lineColor({ slot: i }, isDark) }} /></Table.Cell>
                      <Table.Cell className="max-w-0 truncate" title={seriesLabel(s.labels, agg ?? "")}>{seriesLabel(s.labels, agg ?? "")}<span className="ml-2 text-kumo-subtle">({v.length})</span></Table.Cell>
                      <Table.Cell className="text-right">{format(last)}</Table.Cell>
                      <Table.Cell className="text-right">{format(Math.min(...v))}</Table.Cell>
                      <Table.Cell className="text-right">{format(v.reduce((a, b) => a + b, 0) / (v.length || 1))}</Table.Cell>
                      <Table.Cell className="text-right">{format(Math.max(...v))}</Table.Cell>
                    </Table.Row>
                  );
                })}
              </Table.Body>
            </DataTable>
          </Section>
        </>
      )}
    </Page>
  );
}
