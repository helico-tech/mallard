import { useMemo } from "react";
import { Table } from "@cloudflare/kumo";
import { ChartLineIcon } from "@phosphor-icons/react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import { otel } from "@/api/otel";
import type { OperationSummary, ServiceSummary } from "@/api/types";
import { useInterval } from "@/app/settings";
import { useParam, useScope, useWindow } from "@/app/window";
import { ChartLegend, TimeChart } from "@/components/charts-kumo";
import { useListNav } from "@/components/list";
import { Ago, DataTable, EmptyState, ErrorState, Level, Pad, Page, Section, Service, StaleNote, Stat, TableSkeleton } from "@/components/ui-bits";
import { NoErrors, NoTelemetry, WidenButton } from "@/components/scope-states";
import { EventBars, useProjectStats } from "@/features/errors/IssuesPage";
import { sentry } from "@/api/sentry";
import { clean } from "@/lib/clean";
import { rateClass, windowSeconds } from "@/lib/domain";
import { compact, ms, percent } from "@/lib/format";
import { setToken } from "@/lib/query";
import { cn } from "@/lib/utils";

const RED_HEADERS: [string, string][] = [
  ["Rate", "w-24"],
  ["Errors", "w-24"],
  ["p50", "hidden w-24 @2xl:table-cell"],
  ["p95", "w-24"],
  ["p99", "hidden w-24 @xl:table-cell"],
  ["Spans", "hidden w-20 @3xl:table-cell"],
];

function RedCells({ s }: { s: ServiceSummary | OperationSummary }) {
  return (
    <>
      <Table.Cell className="text-right">{compact(s.rate_per_sec)}/s</Table.Cell>
      <Table.Cell className={cn("text-right", rateClass(s.error_rate))}>{percent(s.error_rate)}</Table.Cell>
      <Table.Cell className="hidden text-right @2xl:table-cell">{ms(s.p50_ms)}</Table.Cell>
      <Table.Cell className="text-right">{ms(s.p95_ms)}</Table.Cell>
      <Table.Cell className="hidden text-right @xl:table-cell">{ms(s.p99_ms)}</Table.Cell>
      <Table.Cell className="hidden text-right text-kumo-subtle @3xl:table-cell">{compact(s.span_count)}</Table.Cell>
    </>
  );
}

function Services({ w }: { w: string }) {
  const refetchInterval = useInterval(5000);
  const [, setService] = useParam("service");
  const q = useQuery({ queryKey: ["otel", "services", w], queryFn: ({ signal }) => otel.services(w, signal), refetchInterval, placeholderData: keepPreviousData });
  const items = q.data ?? [];
  const nav: { selected: ServiceSummary | undefined; selectedId: string | null; select: (id: string | null) => void; index: number } = useListNav<ServiceSummary>({ listKey: "services", items, idOf: (s) => s.name, onOpen: (s) => setService(s.name), keys: { l: () => nav.selected && setService(nav.selected.name), ArrowRight: () => nav.selected && setService(nav.selected.name) } });
  const { selectedId, select } = nav;
  return (
    <Section className="min-h-72 flex-1 shrink-0" title="Services" right={q.data ? `${items.length} services` : undefined}>
      {q.isError && q.data && <StaleNote error={q.error} />}
      {q.isError && !q.data ? <ErrorState error={q.error} onRetry={() => void q.refetch()} /> : q.isPending ? <TableSkeleton rows={4} /> : items.length === 0 ? (
        <EmptyState title={`No spans in the last ${w}`} description="Send traces to the server over OTLP, or widen the time window." commandLine="mallard setup my-app" />
      ) : (
        <DataTable fixed>
          <caption className="sr-only">Services. Enter opens the operations of a service.</caption>
          <Table.Header variant="compact" sticky><Table.Row><Table.Head>Service</Table.Head>{RED_HEADERS.map(([h, c]) => <Table.Head key={h} className={`${c} text-right`}>{h}</Table.Head>)}</Table.Row></Table.Header>
          <Table.Body>
            {items.map((s) => (
              <Table.Row key={s.name} data-row-id={s.name} aria-selected={selectedId === s.name} variant={selectedId === s.name ? "selected" : "default"} onClick={() => select(s.name)} onDoubleClick={() => setService(s.name)}>
                <Table.Cell><button type="button" onClick={() => setService(s.name)} className="flex w-full min-w-0 font-medium hover:underline"><Service name={s.name} /></button></Table.Cell>
                <RedCells s={s} />
              </Table.Row>
            ))}
          </Table.Body>
        </DataTable>
      )}
    </Section>
  );
}

function Operations({ w, service }: { w: string; service: string }) {
  const navigate = useNavigate();
  const refetchInterval = useInterval(5000);
  const [, setService] = useParam("service");
  const q = useQuery({ queryKey: ["otel", "operations", w, service], queryFn: ({ signal }) => otel.operations(w, [service], signal), refetchInterval, placeholderData: keepPreviousData });
  const items = q.data ?? [];
  const back = () => setService("");
  const open = (o: OperationSummary) => navigate(`/telemetry/traces?f=${encodeURIComponent(setToken(`service=${service}`, "op", o.name))}&w=${w}`);
  const { selectedId, select } = useListNav({ listKey: `ops:${service}`, items, idOf: (o) => `${o.name}|${o.kind}`, onOpen: open, keys: { Escape: back, q: back, Backspace: back, h: back, ArrowLeft: back } });
  return (
    <Section
      className="min-h-72 flex-1 shrink-0"
      title={<span className="inline-flex items-center gap-2">Operations of <Service name={clean(service)} /></span>}
      right={q.data ? `${items.length} operations` : undefined}
     
    >
      {q.isError && q.data && <StaleNote error={q.error} />}
      {q.isError && !q.data ? <ErrorState error={q.error} onRetry={() => void q.refetch()} /> : q.isPending ? <TableSkeleton rows={4} /> : items.length === 0 ? (
        <EmptyState title={`No operations in the last ${w}`} />
      ) : (
        <DataTable fixed>
          <caption className="sr-only">Operations. Enter opens their traces.</caption>
          <Table.Header variant="compact" sticky><Table.Row><Table.Head>Operation</Table.Head><Table.Head className="hidden w-24 @3xl:table-cell">Kind</Table.Head>{RED_HEADERS.map(([h, c]) => <Table.Head key={h} className={`${c} text-right`}>{h}</Table.Head>)}</Table.Row></Table.Header>
          <Table.Body>
            {items.map((o) => {
              const id = `${o.name}|${o.kind}`;
              return (
                <Table.Row key={id} data-row-id={id} aria-selected={selectedId === id} variant={selectedId === id ? "selected" : "default"} onClick={() => select(id)} onDoubleClick={() => open(o)}>
                  <Table.Cell><Link to={`/telemetry/traces?f=${encodeURIComponent(setToken(`service=${service}`, "op", o.name))}&w=${w}`} onClick={(e) => e.stopPropagation()} className="mono block truncate font-medium hover:underline" title={o.name}>{o.name}</Link></Table.Cell>
                  <Table.Cell className="hidden text-kumo-subtle lowercase @3xl:table-cell">{o.kind}</Table.Cell>
                  <RedCells s={o} />
                </Table.Row>
              );
            })}
          </Table.Body>
        </DataTable>
      )}
    </Section>
  );
}

/** The span numbers of a window, for every service or for the app's. */
function useSpanNumbers(w: string, services: string[] | undefined, enabled = true) {
  const refetchInterval = useInterval(5000);
  const secs = windowSeconds(w);
  const q = useQuery({
    queryKey: ["otel", "span-series", w, services],
    enabled,
    queryFn: ({ signal }) => otel.spanSeries(w, services, signal),
    refetchInterval,
    placeholderData: keepPreviousData,
  });
  const pts = useMemo(() => q.data?.points ?? [], [q.data]);
  const step = q.data?.step_secs || 1;
  const count = pts.reduce((a, p) => a + p.count, 0);
  const errors = pts.reduce((a, p) => a + p.errors, 0);
  const p95 = count ? pts.reduce((a, p) => a + p.p95_ms * p.count, 0) / count : 0;
  const throughput = useMemo(() => [
    { name: "Requests/s", slot: 0, points: pts.map((p): [number, number] => [Date.parse(p.time), p.count / step]) },
    { name: "Errors/s", slot: 0, attention: true, points: pts.map((p): [number, number] => [Date.parse(p.time), p.errors / step]) },
  ], [pts, step]);
  // A bucket with no spans has no latency; it is a gap, not zero.
  const latency = useMemo(() => {
    const line = (name: string, slot: number, f: (p: (typeof pts)[number]) => number) => ({
      name, slot, points: pts.filter((p) => f(p) > 0).map((p): [number, number] => [Date.parse(p.time), f(p)]),
    });
    return [line("p50", 0, (p) => p.p50_ms), line("p95", 1, (p) => p.p95_ms), line("p99", 2, (p) => p.p99_ms)];
  }, [pts]);
  return { q, secs, pts, count, errors, p95, errorRate: count ? errors / count : 0, throughput, latency };
}

function SpanCharts({ numbers, w }: { numbers: ReturnType<typeof useSpanNumbers>; w: string }) {
  const spanMs = windowSeconds(w) * 1000;
  return (
    <div className="grid shrink-0 gap-4 lg:grid-cols-[1.2fr_1fr]">
      <Section title="Throughput" right={<ChartLegend lines={numbers.throughput} />}><Pad><TimeChart lines={numbers.throughput} height={200} format={compact} spanMs={spanMs} /></Pad></Section>
      <Section title="Latency" right={<ChartLegend lines={numbers.latency} />}><Pad><TimeChart lines={numbers.latency} height={200} format={(v) => (v === 0 ? "0" : ms(v))} spanMs={spanMs} /></Pad></Section>
    </div>
  );
}

/** Failing first, then slowest. */
function worstFirst(ops: OperationSummary[]): OperationSummary[] {
  return [...ops].sort((a, b) => b.error_rate - a.error_rate || b.p95_ms - a.p95_ms).slice(0, 8);
}

function WorstOperations({ w, services }: { w: string; services: string[] }) {
  const navigate = useNavigate();
  const refetchInterval = useInterval(5000);
  const q = useQuery({
    queryKey: ["otel", "operations", w, services],
    queryFn: ({ signal }) => otel.operations(w, services, signal),
    refetchInterval,
    placeholderData: keepPreviousData,
  });
  const items = useMemo(() => worstFirst(q.data ?? []), [q.data]);
  const href = (o: OperationSummary) => `/telemetry/traces?f=${encodeURIComponent(setToken("", "op", o.name))}`;
  const { selectedId, select } = useListNav({ listKey: `worst:${services.join()}`, items, idOf: (o) => `${o.name}|${o.kind}`, onOpen: (o) => navigate(href(o)) });
  return (
    <Section title="Worst operations" right={q.data ? "failing first, then slowest" : undefined}>
      {q.isError && q.data && <StaleNote error={q.error} />}
      {q.isError && !q.data ? <ErrorState error={q.error} onRetry={() => void q.refetch()} /> : q.isPending ? <TableSkeleton rows={4} /> : items.length === 0 ? (
        <EmptyState title={`No operations in the last ${w}`} action={<WidenButton />} />
      ) : (
        <DataTable fixed>
          <caption className="sr-only">Worst operations. Enter opens their traces.</caption>
          <Table.Header variant="compact" sticky><Table.Row><Table.Head>Operation</Table.Head><Table.Head className="w-20 text-right">Errors</Table.Head><Table.Head className="w-20 text-right">p95</Table.Head><Table.Head className="hidden w-20 text-right @lg:table-cell">Rate</Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {items.map((o) => {
              const id = `${o.name}|${o.kind}`;
              return (
                <Table.Row key={id} data-row-id={id} aria-selected={selectedId === id} variant={selectedId === id ? "selected" : "default"} onClick={() => select(id)} onDoubleClick={() => navigate(href(o))}>
                  <Table.Cell><Link to={href(o)} onClick={(e) => e.stopPropagation()} className="mono block truncate font-medium hover:underline" title={o.name}>{o.name}</Link></Table.Cell>
                  <Table.Cell className={cn("text-right", rateClass(o.error_rate))}>{percent(o.error_rate)}</Table.Cell>
                  <Table.Cell className="text-right">{ms(o.p95_ms)}</Table.Cell>
                  <Table.Cell className="hidden text-right @lg:table-cell">{compact(o.rate_per_sec)}/s</Table.Cell>
                </Table.Row>
              );
            })}
          </Table.Body>
        </DataTable>
      )}
    </Section>
  );
}

function NewIssues({ project, w }: { project: string; w: string }) {
  const refetchInterval = useInterval(5000);
  const q = useQuery({
    queryKey: ["sentry", "new-issues", project, w],
    queryFn: ({ signal }) => sentry.issues(project, { status: "unresolved", since: w, sort: "first_seen", new: true, limit: 8 }, signal),
    refetchInterval,
    placeholderData: keepPreviousData,
  });
  const items = q.data?.issues ?? [];
  const slug = encodeURIComponent(project);
  return (
    <Section title="New issues" right={<Link to={`/errors/${slug}?sort=first_seen`} className="hover:underline">All issues</Link>}>
      {q.isError && q.data && <StaleNote error={q.error} />}
      {q.isError && !q.data ? <ErrorState error={q.error} onRetry={() => void q.refetch()} /> : q.isPending ? <TableSkeleton rows={4} /> : items.length === 0 ? (
        <EmptyState title={`No new issues in the last ${w}`} action={<WidenButton />} />
      ) : (
        <DataTable fixed>
          <caption className="sr-only">Issues first seen in this time window, newest first.</caption>
          <Table.Header variant="compact" sticky><Table.Row><Table.Head className="hidden w-24 @lg:table-cell">Level</Table.Head><Table.Head>Issue</Table.Head><Table.Head className="w-20 text-right">Events</Table.Head><Table.Head className="w-24">Last seen</Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {items.map((i) => (
              <Table.Row key={i.id}>
                <Table.Cell className="hidden @lg:table-cell"><Level level={i.level} /></Table.Cell>
                <Table.Cell><Link to={`/errors/${slug}/issues/${i.id}`} className="block truncate font-medium hover:underline" title={i.title}>{i.title}</Link></Table.Cell>
                <Table.Cell className="text-right">{compact(i.count)}</Table.Cell>
                <Table.Cell><Ago t={i.last_seen} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </DataTable>
      )}
    </Section>
  );
}

/** One app, one look: is it healthy? Every number leads to what it counts, and an app
 *  with one side shows that side and says how to add the other (story 0004/05). */
function AppHome() {
  const { app, info, services, project, loading, error } = useScope();
  const { w } = useWindow();
  const telemetry = !!services?.length;
  const numbers = useSpanNumbers(w, services ?? undefined, telemetry);
  const stats = useProjectStats(project, w);
  const s = stats.data;
  const slug = encodeURIComponent(app);

  if (loading) return <Page><TableSkeleton /></Page>;
  if (error && !info) return <Page><ErrorState error={error} title="Could not load the apps" /></Page>;
  if (!telemetry && !project) {
    return (
      <Page>
        <Section><NoTelemetry /></Section>
      </Page>
    );
  }
  return (
    <Page scroll>
      <h1 className="shrink-0 text-lg font-semibold [overflow-wrap:anywhere]">{info?.name ?? clean(app)}</h1>
      <Section>
        <div className="grid grid-cols-[repeat(auto-fit,minmax(160px,1fr))] divide-x divide-kumo-hairline">
          {telemetry && <Stat label="Error rate" href="/telemetry/traces?f=errors" value={numbers.q.data ? percent(numbers.errorRate) : "–"} valueClass={numbers.q.data ? rateClass(numbers.errorRate) : undefined} sub={numbers.q.data ? `${compact(numbers.errors)} failed` : " "} />}
          {telemetry && <Stat label="p95 latency" href="/telemetry/traces" value={numbers.q.data ? ms(numbers.p95) : "–"} sub="avg of per-bucket p95" />}
          {telemetry && <Stat label="Requests" href="/telemetry/traces" value={numbers.q.data ? `${compact(numbers.count / numbers.secs)}/s` : "–"} sub={numbers.q.data ? `${compact(numbers.count)} total` : " "} />}
          {project && <Stat label="New issues" href={`/errors/${slug}?sort=first_seen`} value={s ? compact(s.new_issues) : "–"} valueClass={s && s.new_issues > 0 ? "text-kumo-warning" : undefined} sub="first seen" />}
          {project && <Stat label="Events" href={`/errors/${slug}`} value={s ? compact(s.total_events) : "–"} sub={s ? `in ${compact(s.unique_issues)} ${s.unique_issues === 1 ? "issue" : "issues"}` : " "} />}
        </div>
      </Section>
      {telemetry && (numbers.q.isError && !numbers.q.data ? <ErrorState error={numbers.q.error} onRetry={() => void numbers.q.refetch()} title="Could not load the charts" /> : <SpanCharts numbers={numbers} w={w} />)}
      {project && <Section title="Events"><Pad>{stats.isError && !s ? <p className="text-kumo-danger">{stats.error.message}</p> : <EventBars stats={s} w={w} />}</Pad></Section>}
      <div className="grid shrink-0 items-start gap-4 lg:grid-cols-2">
        {project ? <NewIssues project={project} w={w} /> : <Section title="Errors"><NoErrors /></Section>}
        {telemetry ? <WorstOperations w={w} services={services!} /> : <Section title="Telemetry"><NoTelemetry /></Section>}
      </div>
    </Page>
  );
}

function AllApps() {
  const { w } = useWindow();
  const [service] = useParam("service");
  const refetchInterval = useInterval(5000);
  const numbers = useSpanNumbers(w, undefined);
  const { q: spans, secs, pts, count, errors, p95, errorRate } = numbers;
  const logs = useQuery({ queryKey: ["otel", "log-series", w], queryFn: ({ signal }) => otel.logSeries({ since: w }, signal), refetchInterval, placeholderData: keepPreviousData });
  const stats = useQuery({ queryKey: ["otel", "stats"], queryFn: ({ signal }) => otel.stats(signal), refetchInterval });

  const logPts = logs.data?.points ?? [];
  const logTotal = logPts.reduce((a, p) => a + p.total, 0);
  const logErrors = logPts.reduce((a, p) => a + p.error, 0);
  const ingest = stats.data?.ingest;
  const bufferPct = ingest && ingest.max_buffer_bytes ? ingest.buffered_bytes / ingest.max_buffer_bytes : 0;

  return (
    <Page scroll>
      <Section>
        <div className="grid grid-cols-2 divide-x divide-kumo-hairline @2xl:grid-cols-3 @4xl:grid-cols-5">
          <Stat label="Requests" value={spans.data ? `${compact(count / secs)}/s` : "–"} sub={spans.data ? `${compact(count)} total` : " "} />
          <Stat label="Errors" value={spans.data ? percent(errorRate) : "–"} valueClass={spans.data ? rateClass(errorRate) : undefined} sub={spans.data ? `${compact(errors)} failed` : " "} />
          <Stat label="p95 latency" value={spans.data ? ms(p95) : "–"} sub="avg of per-bucket p95" />
          <Stat label="Logs" value={logs.data ? `${compact(logTotal / secs)}/s` : "–"} sub={logs.data ? `${compact(logErrors)} errors` : " "} />
          <Stat
            label="Ingest buffer"
            value={ingest ? `${(bufferPct * 100).toFixed(0)}%` : "–"}
            valueClass={bufferPct > 0.8 ? "text-kumo-danger" : bufferPct > 0.5 ? "text-kumo-warning" : undefined}
            sub={ingest ? (ingest.flush_errors ? `${compact(ingest.flush_errors)} flush errors` : `${compact(ingest.flushes)} flushes`) : " "}
          />
        </div>
      </Section>
      {spans.isError && !spans.data ? (
        <ErrorState error={spans.error} onRetry={() => void spans.refetch()} title="Could not load the charts" />
      ) : pts.length === 0 && !spans.isPending ? (
        <Section><EmptyState icon={<ChartLineIcon size={32} />} title={`No data in the last ${w}`} description="Widen the time window, or send telemetry to the server." /></Section>
      ) : (
        <SpanCharts numbers={numbers} w={w} />
      )}
      {service ? <Operations w={w} service={service} /> : <Services w={w} />}
    </Page>
  );
}

export default function OverviewPage() {
  const { app } = useScope();
  return app ? <AppHome /> : <AllApps />;
}
