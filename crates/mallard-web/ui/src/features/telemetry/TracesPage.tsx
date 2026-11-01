import { useMemo } from "react";
import { Button, useKumoToastManager, Table } from "@cloudflare/kumo";
import { WarningIcon, XIcon } from "@phosphor-icons/react";
import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import { otel } from "@/api/otel";
import { sentry } from "@/api/sentry";
import type { TraceSummary } from "@/api/types";
import { useInterval } from "@/app/settings";
import { useParam, useWindow } from "@/app/window";
import { useListNav } from "@/components/list";
import { DataTable, EmptyState, ErrorState, Page, SearchField, Section, Service, StaleNote, TableSkeleton } from "@/components/ui-bits";
import { windowSeconds } from "@/lib/domain";
import { compact, dayTime, ms, timeMs } from "@/lib/format";
import { parseTraceFilter, setToken, tokens } from "@/lib/query";
import { cn } from "@/lib/utils";

function toggleErrors(filter: string, on: boolean): string {
  const rest = tokens(filter).filter((t) => !/^(errors?|err)(=.*)?$/i.test(t));
  const quoted = rest.map((t) => (t.includes(" ") ? t.replace(/=(.*)$/, '="$1"') : t)).join(" ");
  return on ? quoted : `errors${quoted ? ` ${quoted}` : ""}`;
}

export function TracesPage() {
  const { w } = useWindow("telemetry");
  const [f, setF] = useParam("f");
  const navigate = useNavigate();
  const refetchInterval = useInterval(5000);
  const toasts = useKumoToastManager();

  const parsed = parseTraceFilter(f);
  const filter = "error" in parsed ? { errorsOnly: false, attributes: [] as string[] } : parsed;
  const params = {
    service: "service" in filter ? filter.service : undefined,
    operation: "operation" in filter ? filter.operation : undefined,
    errors_only: filter.errorsOnly,
    min_duration_ms: "minMs" in filter ? filter.minMs : undefined,
    max_duration_ms: "maxMs" in filter ? filter.maxMs : undefined,
    attributes: filter.attributes.join(",") || undefined,
  };

  const q = useInfiniteQuery({
    queryKey: ["otel", "traces", w, f],
    queryFn: async ({ signal, pageParam }) => {
      const at = Date.now();
      const page = await otel.traces(
        pageParam ? { ...params, cursor: pageParam.cursor, from: pageParam.from, to: pageParam.to } : { ...params, since: w },
        signal,
      );
      return { ...page, at, window: pageParam ?? { from: new Date(at - windowSeconds(w) * 1000).toISOString(), to: new Date(at).toISOString() } };
    },
    initialPageParam: null as null | { cursor: string; from: string; to: string },
    getNextPageParam: (last) => (last.nextCursor ? { cursor: last.nextCursor, from: last.window.from, to: last.window.to } : undefined),
    refetchInterval,
    placeholderData: keepPreviousData,
  });

  const items = useMemo(() => {
    const seen = new Set<string>();
    return (q.data?.pages ?? []).flatMap((p) => p.data).filter((t) => (seen.has(t.trace_id) ? false : (seen.add(t.trace_id), true)));
  }, [q.data]);
  const maxDuration = Math.max(1, ...items.map((t) => t.duration_ms));
  const longWindow = windowSeconds(w) > 86400;

  // Sentry errors per trace, one call per list (`ErrorLookup` in the TUI).
  const ids = items.slice(0, 200).map((t) => t.trace_id);
  const errorsQ = useQuery({
    queryKey: ["sentry", "errors-by-trace", w, ids.join(",")],
    queryFn: ({ signal }) => {
      const to = new Date();
      return sentry.errorsByTrace(ids, new Date(to.getTime() - windowSeconds(w) * 1000).toISOString(), to.toISOString(), signal);
    },
    enabled: ids.length > 0,
    refetchInterval,
    placeholderData: keepPreviousData,
  });
  // Unknown is not zero: when Sentry cannot be read the column says so.
  const errorsOf = (id: string): number | null => (errorsQ.data ? (errorsQ.data.traces[id]?.count ?? 0) : null);

  const open = (t: TraceSummary) => navigate(`/telemetry/traces/${t.trace_id}?hint=${encodeURIComponent(t.start_time)}`);
  const nav: { selected: TraceSummary | undefined; selectedId: string | null; select: (id: string | null) => void; index: number } = useListNav<TraceSummary>({
    listKey: "traces",
    items,
    idOf: (t) => t.trace_id,
    onOpen: open,
    keys: {
      E: () => {
        const next = toggleErrors(f, filter.errorsOnly);
        setF(next);
        toasts.add({ title: filter.errorsOnly ? "All traces" : "Only traces with errors" });
      },
      c: () => setF(""),
      ArrowRight: () => nav.selected && open(nav.selected),
      x: () => nav.selected && navigate(`/errors-by-trace/${nav.selected.trace_id}`),
      l: () => nav.selected && navigate(`/telemetry/logs?f=${encodeURIComponent(setToken("", "trace", nav.selected.trace_id))}&w=${w}`),
    },
  });
  const { selectedId, select } = nav;

  return (
    <Page>
      <div className="flex shrink-0 flex-wrap items-start gap-2">
        <SearchField
          value={f}
          onApply={setF}
          mono
          label="Filter traces"
          placeholder='service=api op="GET /x" min=200ms errors http.route=/y'
          validate={(v) => {
            const p = parseTraceFilter(v);
            return "error" in p ? p.error : null;
          }}
        />
        {f && <Button variant="ghost" icon={<XIcon />} onClick={() => setF("")}>Clear</Button>}
        <Button variant={filter.errorsOnly ? "primary" : "secondary"} aria-pressed={filter.errorsOnly} onClick={() => setF(toggleErrors(f, filter.errorsOnly))}>Errors only</Button>
      </div>
      <Section className="min-h-0 flex-1 shrink!" title="Traces" right={q.data ? `${errorsQ.isError ? "Sentry errors unavailable, " : ""}${compact(items.length)} newest${q.hasNextPage ? ", more below" : ""}` : undefined}>
        {q.isError && q.data && <StaleNote error={q.error} />}
        {q.isError && !q.data ? (
          <ErrorState error={q.error} onRetry={() => void q.refetch()} title="Could not load traces" />
        ) : q.isPending ? (
          <TableSkeleton />
        ) : items.length === 0 ? (
          <EmptyState
            title={`No matching traces in the last ${w}`}
            description={f ? "Change the filter, or widen the time window." : "Send traces to the server over OTLP, or widen the time window."}
            action={f ? <Button onClick={() => setF("")}>Clear the filter</Button> : undefined}
          />
        ) : (
          <div className="min-h-0 flex-1 overflow-auto" aria-busy={q.isFetching}>
            <DataTable fixed>
              <caption className="sr-only">Traces, newest first. Enter opens one.</caption>
              <Table.Header variant="compact" sticky>
                <Table.Row>
                  <Table.Head className="hidden w-32 @2xl:table-cell">Start</Table.Head>
                  <Table.Head>Root</Table.Head>
                  <Table.Head className="w-24 text-right">Duration</Table.Head>
                  <Table.Head className="hidden w-32 @3xl:table-cell"><span className="sr-only">Duration bar</span></Table.Head>
                  <Table.Head className="hidden w-20 text-right @xl:table-cell">Spans</Table.Head>
                  <Table.Head className="hidden w-20 text-right @lg:table-cell">Errors</Table.Head>
                  <Table.Head className="hidden w-20 text-right @lg:table-cell">Sentry</Table.Head>
                  <Table.Head className="hidden w-[24%] @6xl:table-cell">Services</Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {items.map((t) => {
                  const sentryCount = errorsOf(t.trace_id);
                  const failed = t.error_count > 0;
                  return (
                    <Table.Row key={t.trace_id} data-row-id={t.trace_id} aria-selected={selectedId === t.trace_id} variant={selectedId === t.trace_id ? "selected" : "default"} onClick={() => select(t.trace_id)} onDoubleClick={() => open(t)}>
                      <Table.Cell className="hidden text-kumo-subtle @2xl:table-cell">{longWindow ? dayTime(t.start_time) : timeMs(t.start_time)}</Table.Cell>
                      <Table.Cell className="max-w-0">
                        <Link to={`/telemetry/traces/${t.trace_id}?hint=${encodeURIComponent(t.start_time)}`} onClick={(e) => e.stopPropagation()} className="flex min-w-0 items-center gap-2 hover:underline">
                          {t.root_service ? <Service name={t.root_service} className="max-w-[40%] shrink-0 text-kumo-subtle" /> : t.services[0] ? <Service name={t.services[0]} className="max-w-[40%] shrink-0 text-kumo-subtle" /> : null}
                          <span className={cn("truncate font-medium", failed ? "text-kumo-danger" : "text-kumo-default")} title={t.root_name ?? undefined}>{t.root_name ?? "(root not received)"}</span>
                        </Link>
                      </Table.Cell>
                      <Table.Cell className="text-right">{ms(t.duration_ms)}</Table.Cell>
                      <Table.Cell className="hidden @3xl:table-cell">
                        <div className="h-1.5 rounded-full bg-kumo-recessed" aria-hidden>
                          <div className={cn("h-1.5 rounded-full", failed ? "bg-kumo-danger" : "bg-kumo-brand")} style={{ width: `${Math.max(2, (t.duration_ms / maxDuration) * 100)}%` }} />
                        </div>
                      </Table.Cell>
                      <Table.Cell className="hidden text-right text-kumo-subtle @xl:table-cell">{compact(t.span_count)}</Table.Cell>
                      <Table.Cell className="hidden text-right @lg:table-cell">{failed ? <span className="font-medium text-kumo-danger">{compact(t.error_count)}</span> : <span className="text-kumo-subtle">0</span>}</Table.Cell>
                      <Table.Cell className="hidden text-right @lg:table-cell">
                        {sentryCount === null ? <span className="text-kumo-subtle" title={errorsQ.error?.message ?? "Loading"}>–</span> : sentryCount > 0 ? <span className="inline-flex items-center justify-end gap-1 font-medium text-kumo-warning"><WarningIcon aria-label="Sentry errors" />{compact(sentryCount)}</span> : <span className="text-kumo-subtle">0</span>}
                      </Table.Cell>
                      <Table.Cell className="hidden truncate text-kumo-subtle @6xl:table-cell" title={t.services.join(", ")}>{t.services.join(", ")}</Table.Cell>
                    </Table.Row>
                  );
                })}
              </Table.Body>
            </DataTable>
            {q.hasNextPage ? (
              <div className="flex justify-center px-4 py-3"><Button variant="secondary" loading={q.isFetchingNextPage} onClick={() => void q.fetchNextPage()}>Load more</Button></div>
            ) : (
              <p className="px-4 py-3 text-center text-xs text-kumo-subtle">End of the list</p>
            )}
          </div>
        )}
      </Section>
    </Page>
  );
}
