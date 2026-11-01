import { useMemo } from "react";
import { Table } from "@cloudflare/kumo";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import { otel } from "@/api/otel";
import type { MetricInfo } from "@/api/types";
import { useInterval } from "@/app/settings";
import { useParam, useWindow } from "@/app/window";
import { useListNav } from "@/components/list";
import { DataTable, EmptyState, ErrorState, Page, SearchField, Section, StaleNote, TableSkeleton } from "@/components/ui-bits";
import { compact } from "@/lib/format";
import { metricTypeLabel } from "@/lib/metrics";

export function MetricsPage() {
  const { w } = useWindow("telemetry");
  const [search, setSearch] = useParam("q");
  const navigate = useNavigate();
  const refetchInterval = useInterval(30000);
  const q = useQuery({ queryKey: ["otel", "metrics", w], queryFn: ({ signal }) => otel.metrics(w, signal), refetchInterval, placeholderData: keepPreviousData });
  const all = useMemo(() => q.data ?? [], [q.data]);
  const items = useMemo(() => {
    const needle = search.toLowerCase();
    return needle ? all.filter((m) => m.name.toLowerCase().includes(needle)) : all;
  }, [all, search]);
  const href = (m: MetricInfo) => `/telemetry/metrics/${encodeURIComponent(m.name)}?w=${w}`;
  const { selectedId, select, selected } = useListNav({
    listKey: "metrics",
    items,
    idOf: (m) => m.name,
    onOpen: (m) => navigate(href(m)),
    keys: {
      Escape: () => search && setSearch(""),
      ArrowRight: () => selected && navigate(href(selected)),
    },
  });
  return (
    <Page>
      <div className="flex shrink-0">
        <SearchField value={search} onApply={(v) => setSearch(v, true)} live label="Search metrics" placeholder="Search metrics" />
      </div>
      <Section className="min-h-0 flex-1 shrink!" title="Metrics" right={q.data ? `${compact(items.length)} of ${compact(all.length)}` : undefined}>
        {q.isError && q.data && <StaleNote error={q.error} />}
        {q.isError && !q.data ? (
          <ErrorState error={q.error} onRetry={() => void q.refetch()} title="Could not load metrics" />
        ) : q.isPending ? (
          <TableSkeleton />
        ) : items.length === 0 ? (
          <EmptyState title={search ? "No metric matches" : `No metrics in the last ${w}`} description={search ? "Change the search." : "Send metrics to the server over OTLP, or widen the time window."} />
        ) : (
          <div className="min-h-0 flex-1 overflow-auto">
            <DataTable>
              <caption className="sr-only">Metrics. Enter opens a chart.</caption>
              <Table.Header variant="compact" sticky><Table.Row><Table.Head>Metric</Table.Head><Table.Head className="hidden w-28 @lg:table-cell">Type</Table.Head><Table.Head className="hidden w-20 @2xl:table-cell">Unit</Table.Head><Table.Head className="hidden w-20 text-right @xl:table-cell">Series</Table.Head><Table.Head className="hidden w-20 text-right @2xl:table-cell">Points</Table.Head><Table.Head className="hidden @4xl:table-cell">Services</Table.Head></Table.Row></Table.Header>
              <Table.Body>
                {items.map((m) => (
                  <Table.Row key={m.name} data-row-id={m.name} aria-selected={selectedId === m.name} variant={selectedId === m.name ? "selected" : "default"} onClick={() => select(m.name)} onDoubleClick={() => navigate(href(m))}>
                    <Table.Cell className="max-w-0"><Link to={href(m)} onClick={(e) => e.stopPropagation()} className="mono block truncate font-medium hover:underline" title={m.description ? `${m.name}: ${m.description}` : m.name}>{m.name}</Link></Table.Cell>
                    <Table.Cell className="hidden text-kumo-subtle @lg:table-cell">{metricTypeLabel(m.metric_type)}</Table.Cell>
                    <Table.Cell className="mono hidden text-kumo-subtle @2xl:table-cell">{m.unit || "–"}</Table.Cell>
                    <Table.Cell className="hidden text-right @xl:table-cell">{compact(m.series_count)}</Table.Cell>
                    <Table.Cell className="hidden text-right text-kumo-subtle @2xl:table-cell">{compact(m.point_count)}</Table.Cell>
                    <Table.Cell className="hidden max-w-0 truncate text-kumo-subtle @4xl:table-cell" title={m.services.join(", ")}>{m.services.join(", ")}</Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </DataTable>
          </div>
        )}
      </Section>
    </Page>
  );
}
