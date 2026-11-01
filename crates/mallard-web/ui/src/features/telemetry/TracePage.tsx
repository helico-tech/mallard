import { useMemo, useRef, useState } from "react";
import { Button, LayerCard, Link as KumoLink, useKumoToastManager } from "@cloudflare/kumo";
import { CaretDownIcon, CaretRightIcon, WarningIcon } from "@phosphor-icons/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError } from "@/api/client";
import { otel } from "@/api/otel";
import { sentry } from "@/api/sentry";
import type { LogRecord, Span, TraceEvent } from "@/api/types";
import { useKeys } from "@/app/keys";
import { useParam } from "@/app/window";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import { useBack, useNarrow, Severity, Attributes, CopyId, DetailHeader, LIST_HEADER, EmptyState, ErrorState, Facts, Page, Service, TableSkeleton } from "@/components/ui-bits";
import { serviceColor, severityName } from "@/lib/domain";
import { compact, ms, timeMs, timestampFull } from "@/lib/format";
import { parseTraceId } from "@/lib/query";
import { barGeometry, buildTree, epochMicros, type Row } from "@/lib/waterfall";
import { cn } from "@/lib/utils";

const HOUR = 3_600_000;
const ROW_H = 32;
/** A trace can still be arriving: look again a few times (as the TUI does). */
const RETRY_DELAYS = [3000, 6000, 12000, 24000, 48000];

function useTraceWindow(hint: string | null) {
  const t = hint ? Date.parse(hint) : NaN;
  if (Number.isNaN(t)) return { since: "7d" };
  return { from: new Date(t - HOUR).toISOString(), to: new Date(t + HOUR).toISOString() };
}

function WaterfallRow({ row, totalMs, selected, sentryCount, collapsed, onSelect, onToggle }: {
  row: Row<Span>;
  totalMs: number;
  selected: boolean;
  sentryCount: number;
  collapsed: boolean;
  onSelect: () => void;
  onToggle: () => void;
}) {
  const s = row.span;
  const failed = s.status_code.toLowerCase() === "error";
  const { left, width } = barGeometry(row.offsetMs, row.durationMs, totalMs);
  return (
    <div
      role="row"
      aria-selected={selected}
      data-row-id={s.span_id}
      onClick={onSelect}
      className={cn("grid h-full cursor-pointer grid-cols-[minmax(0,60%)_minmax(0,1fr)_72px] items-center gap-2 border-b border-kumo-hairline pr-4 @2xl:grid-cols-[minmax(0,45%)_minmax(0,1fr)_72px]", selected ? "row-selected" : "hover:bg-kumo-tint")}
    >
      <div role="gridcell" className="flex min-w-0 items-center" style={{ paddingLeft: Math.min(row.depth, 16) * 12 + 10 }}>
        {row.hasChildren ? (
          <button
            type="button"
            aria-label={`${collapsed ? "Expand" : "Collapse"} ${row.descendants} spans below`}
            aria-expanded={!collapsed}
            onClick={(e) => {
              e.stopPropagation();
              onToggle();
            }}
            className="grid size-6 shrink-0 place-items-center rounded text-kumo-subtle hover:bg-kumo-recessed"
          >
            {collapsed ? <CaretRightIcon size={12} /> : <CaretDownIcon size={12} />}
          </button>
        ) : (
          <span className="size-6 shrink-0" />
        )}
        <span aria-hidden className="mr-1.5 size-2 shrink-0 rounded-full" style={{ background: serviceColor(s.service_name) }} />
        <span className="mr-2 hidden max-w-[30%] shrink-0 truncate text-kumo-subtle @3xl:block" title={s.service_name}>{s.service_name}</span>
        {sentryCount > 0 && <WarningIcon aria-label={`${sentryCount} Sentry errors`} className="mr-1 shrink-0 text-kumo-warning" />}
        {row.orphan && <span className="mr-2 shrink-0 text-xs text-kumo-warning" title="The parent span was not received">orphan</span>}
        <span className={cn("truncate", failed ? "font-medium text-kumo-danger" : "text-kumo-default")} title={s.name}>{s.name}</span>
        {collapsed && <span className="tnum ml-1.5 shrink-0 text-xs text-kumo-subtle">+{compact(row.descendants)}</span>}
      </div>
      <div role="gridcell" className="relative h-3" aria-hidden>
        <div
          className={cn("absolute inset-y-0 rounded-[2px]", failed && "bg-kumo-danger")}
          style={{ left: `${left}%`, width: `${width}%`, minWidth: 2, ...(failed ? {} : { background: serviceColor(s.service_name) }) }}
        />
      </div>
      <div role="gridcell" className="tnum text-right">{ms(row.durationMs)}</div>
    </div>
  );
}

function SpanPanel({ span, offsetMs, traceStartMs, errors, logs, onOpenError }: {
  span: Span;
  offsetMs: number;
  traceStartMs: number;
  errors: TraceEvent[];
  logs: LogRecord[];
  onOpenError: () => void;
}) {
  const status = span.status_code.toLowerCase();
  return (
    <div className="flex flex-col gap-4 px-4 py-3" aria-label="Span details">
      <div>
        <h3 className="text-lg font-semibold [overflow-wrap:anywhere]">{span.name}</h3>
        <p className="mt-1 flex flex-wrap items-center gap-x-3 text-kumo-subtle">
          <span className={cn("font-medium", status === "error" ? "text-kumo-danger" : status === "ok" ? "text-kumo-success" : "")}>{status === "unset" ? "Unset" : status === "ok" ? "OK" : "Error"}</span>
          <Service name={span.service_name} />
          <span className="lowercase">{span.kind}</span>
          <span className="tnum">{ms(span.duration_ns / 1e6)} at +{ms(offsetMs)}</span>
        </p>
        {span.status_message && <p className="mt-1 [overflow-wrap:anywhere] text-kumo-danger">{span.status_message}</p>}
      </div>
      {errors.length > 0 && (
        <div className="rounded-lg bg-kumo-warning-tint px-3 py-2">
          <h4 className="mb-1 flex items-center gap-1.5 font-medium"><WarningIcon aria-hidden className="text-kumo-warning" />Sentry errors ({errors.length})</h4>
          <ul className="space-y-1">
            {errors.map((e) => (
              <li key={e.event_id}>
                <p className="[overflow-wrap:anywhere] text-kumo-default">{e.title}</p>
                <p className="mono text-kumo-subtle">issue {e.issue_id}, event {e.event_id.slice(0, 12)}</p>
              </li>
            ))}
          </ul>
          <Button variant="secondary" className="mt-2" onClick={onOpenError}>Open the first one (x)</Button>
        </div>
      )}
      <Facts rows={[
        ["Span", <CopyId value={span.span_id} />],
        ["Parent", span.parent_span_id ? <CopyId value={span.parent_span_id} /> : "–"],
        ["Start", <span className="tnum">{timestampFull(span.start_time)}</span>],
        ["Scope", span.scope_name || "–"],
      ]} />
      <Attributes title="Attributes" attrs={span.attributes} />
      {span.events.length > 0 && (
        <div>
          <h4 className="mb-1 font-medium">Events ({span.events.length})</h4>
          <ul className="space-y-1.5">
            {span.events.map((e, i) => {
              const exception = e.name === "exception";
              const stack = e.attributes["exception.stacktrace"];
              const others = Object.fromEntries(Object.entries(e.attributes).filter(([k]) => k !== "exception.stacktrace"));
              return (
                <li key={i}>
                  <p className={cn(exception && "font-medium text-kumo-danger")}><span className="tnum text-kumo-subtle">+{ms(Math.max(0, epochMicros(e.time) / 1000 - traceStartMs))}</span> {e.name}</p>
                  {Object.keys(others).length > 0 && <Facts className="mono mt-0.5 pl-3" rows={Object.entries(others)} />}
                  {stack && <pre className="mono mt-1 overflow-x-auto rounded-md bg-kumo-recessed px-3 py-2 text-kumo-danger">{stack}</pre>}
                </li>
              );
            })}
          </ul>
        </div>
      )}
      {span.links.length > 0 && (
        <div>
          <h4 className="mb-1 font-medium">Links ({span.links.length})</h4>
          <ul className="space-y-0.5">
            {span.links.map((l, i) => (
              <li key={i} className="mono"><KumoLink href={`/telemetry/traces/${l.trace_id}?span=${l.span_id}`}>{l.trace_id.slice(0, 16)} / {l.span_id}</KumoLink></li>
            ))}
          </ul>
        </div>
      )}
      {logs.length > 0 && (
        <div>
          <h4 className="mb-1 font-medium">Logs ({logs.length})</h4>
          <ul className="space-y-0.5">
            {logs.map((l, i) => {
              const name = severityName(l.severity_number, l.severity_text);
              return (
                <li key={i} className="grid grid-cols-[96px_48px_minmax(0,1fr)] items-baseline gap-2">
                  <span className="tnum text-kumo-subtle">{timeMs(l.time)}</span>
                  <Severity name={name} />
                  <span className="mono break-words">{l.body}</span>
                </li>
              );
            })}
          </ul>
        </div>
      )}
      <Attributes title="Resource" attrs={span.resource_attributes} />
    </div>
  );
}

export function TracePage() {
  const { traceId: rawId = "" } = useParams();
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const client = useQueryClient();
  const toasts = useKumoToastManager();
  const [spanParam, setSpanParam] = useParam("span");
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const traceId = parseTraceId(rawId);
  const narrow = useNarrow();
  const hint = params.get("hint");
  const win = useTraceWindow(hint);

  const q = useQuery({
    queryKey: ["otel", "trace", traceId, hint],
    enabled: !!traceId,
    queryFn: async ({ signal }) => {
      try {
        return await otel.trace(traceId!, win, signal);
      } catch (e) {
        // The server may cap windows: fall back to a smaller one.
        if (e instanceof ApiError && e.kind === "usage" && "since" in win) return otel.trace(traceId!, { since: "24h" }, signal);
        throw e;
      }
    },
    retry: (count, e) => e instanceof ApiError && e.kind === "not_found" && count < RETRY_DELAYS.length,
    retryDelay: (attempt) => RETRY_DELAYS[attempt] ?? 48000,
  });
  const spans = useMemo(() => q.data?.spans ?? [], [q.data]);

  const logsQ = useQuery({
    queryKey: ["otel", "trace-logs", traceId, hint],
    enabled: !!traceId,
    queryFn: ({ signal }) => otel.logs({ ...win, trace_id: traceId!, limit: 2000, order: "asc" }, signal),
  });
  const logs = useMemo(() => logsQ.data ?? [], [logsQ.data]);

  const full = useMemo(() => buildTree(spans), [spans]);
  const tree = useMemo(() => buildTree(spans, collapsed), [spans, collapsed]);
  const rows = tree.rows;

  // Sentry errors: first span -1h to trace end +1h.
  const errorsQ = useQuery({
    queryKey: ["sentry", "errors-of-trace", traceId, full.startMs],
    enabled: !!traceId && spans.length > 0,
    queryFn: ({ signal }) =>
      sentry.errorsByTrace(
        [traceId!],
        new Date(full.startMs - HOUR).toISOString(),
        new Date(full.startMs + full.totalMs + HOUR).toISOString(),
        signal,
      ),
  });
  const errorEvents = useMemo(() => errorsQ.data?.traces[traceId ?? ""]?.events ?? [], [errorsQ.data, traceId]);
  const errorCount = errorsQ.data?.traces[traceId ?? ""]?.count ?? 0;
  const errorsBySpan = useMemo(() => {
    const m = new Map<string, TraceEvent[]>();
    for (const e of errorEvents) if (e.span_id) m.set(e.span_id, [...(m.get(e.span_id) ?? []), e]);
    return m;
  }, [errorEvents]);
  const logsBySpan = useMemo(() => {
    const m = new Map<string, LogRecord[]>();
    for (const l of logs) if (l.span_id) m.set(l.span_id, [...(m.get(l.span_id) ?? []), l]);
    return m;
  }, [logs]);

  const selectedIndex = Math.max(0, rows.findIndex((r) => r.span.span_id === spanParam));
  const selected = rows[selectedIndex];
  const selectedId = selected?.span.span_id;

  const scrollRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const virt = useVirtualizer({ count: rows.length, getScrollElement: () => scrollRef.current, estimateSize: () => ROW_H, overscan: 12 });

  const go = (i: number) => {
    const next = rows[Math.max(0, Math.min(rows.length - 1, i))];
    if (!next) return;
    setSpanParam(next.span.span_id, true);
    virt.scrollToIndex(Math.max(0, Math.min(rows.length - 1, i)), { align: "auto" });
    panelRef.current?.scrollTo({ top: 0 });
  };
  const toggle = (id: string) =>
    setCollapsed((c) => {
      const next = new Set(c);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const back = useBack("/telemetry/traces");
  const toErrors = () => {
    const first = selectedId ? errorsBySpan.get(selectedId)?.[0] : undefined;
    navigate(`/errors-by-trace/${traceId}${first ? `?event=${first.event_id}` : ""}`);
  };
  useKeys({
    j: () => go(selectedIndex + 1),
    k: () => go(selectedIndex - 1),
    ArrowDown: () => go(selectedIndex + 1),
    ArrowUp: () => go(selectedIndex - 1),
    g: () => go(0),
    Home: () => go(0),
    G: () => go(rows.length - 1),
    End: () => go(rows.length - 1),
    PageDown: () => panelRef.current?.scrollBy({ top: 200 }),
    "]": () => panelRef.current?.scrollBy({ top: 200 }),
    PageUp: () => panelRef.current?.scrollBy({ top: -200 }),
    "[": () => panelRef.current?.scrollBy({ top: -200 }),
    Escape: back,
    Backspace: back,
    h: back,
    q: back,
    ArrowLeft: back,
    x: toErrors,
    l: () => navigate(`/telemetry/logs?f=${encodeURIComponent(`trace=${traceId}`)}`),
    r: () => {
      void client.invalidateQueries({ queryKey: ["otel", "trace", traceId] });
      void client.invalidateQueries({ queryKey: ["otel", "trace-logs", traceId] });
      toasts.add({ title: "Refreshing" });
    },
  });

  if (!traceId) {
    return <EmptyState title="Not a trace id" description="A trace id is 32 hex characters." action={<Button onClick={() => navigate("/telemetry/traces")}>Show traces</Button>} />;
  }
  const notFound = q.error instanceof ApiError && q.error.kind === "not_found";
  const services = new Set(spans.map((s) => s.service_name)).size;
  const failed = spans.filter((s) => s.status_code.toLowerCase() === "error").length;
  const total = full.totalMs;
  const rootName = full.rows[0]?.span.name;

  return (
    <Page>
      <DetailHeader
        title={rootName ?? "Trace"}
        meta={
          <>
            <CopyId value={traceId} />
            {q.data && (
              <>
                <span>{ms(total)}</span>
                <span>{compact(spans.length)} spans</span>
                <span>{services} services</span>
                {failed > 0 && <span className="font-medium text-kumo-danger">{failed} failed</span>}
              </>
            )}
          </>
        }
        actions={
          q.data && (
            <>
              <Button variant="secondary" icon={errorCount > 0 ? <WarningIcon className="text-kumo-warning" /> : undefined} onClick={toErrors}>{errorCount} Sentry errors</Button>
              <Button variant="secondary" onClick={() => navigate(`/telemetry/logs?f=${encodeURIComponent(`trace=${traceId}`)}`)}>{compact(logs.length)} logs</Button>
            </>
          )
        }
      />
      {q.isError && !notFound ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} title="Could not load this trace" />
      ) : q.isPending || (q.isFetching && !q.data) ? (
        q.failureCount > 0 ? (
          <EmptyState title="This trace has not arrived yet" description="Looking again for a little while. Data shows up a few seconds after it is sent." />
        ) : (
          <TableSkeleton />
        )
      ) : !q.data || spans.length === 0 ? (
        <EmptyState
          title="This trace has no spans"
          description="It may not have arrived yet, or it is older than the time window."
          action={<div className="flex gap-2"><Button onClick={() => void q.refetch()}>Look again</Button><Button variant="secondary" onClick={toErrors}>Errors of this trace</Button></div>}
        />
      ) : (
        <LayerCard className="@container min-h-0 flex-1">
        <ResizablePanelGroup key={narrow ? "v" : "h"} orientation={narrow ? "vertical" : "horizontal"} className="min-h-0 flex-1 bg-kumo-base">
          <ResizablePanel defaultSize="62%" minSize="30%">
            <div className="flex h-full min-h-0 flex-col" role="grid" aria-label="Spans of the trace" aria-rowcount={rows.length}>
              <div role="row" className={cn(LIST_HEADER, "grid-cols-[minmax(0,60%)_minmax(0,1fr)_72px] gap-2 pr-4 @2xl:grid-cols-[minmax(0,45%)_minmax(0,1fr)_72px]")}>
                <span role="columnheader" className="pl-4">Operation</span>
                <span role="columnheader" className="relative font-normal text-kumo-subtle"><span className="absolute left-0">0</span><span className="absolute left-1/2 hidden -translate-x-1/2 @3xl:inline">{ms(total / 2)}</span><span className="absolute right-0">{ms(total)}</span>&nbsp;</span>
                <span role="columnheader" className="text-right">Duration</span>
              </div>
              <div ref={scrollRef} className="min-h-0 flex-1 overflow-auto">
                <div style={{ height: virt.getTotalSize(), position: "relative" }}>
                  {virt.getVirtualItems().map((v) => {
                    const row = rows[v.index]!;
                    return (
                      <div key={row.span.span_id} aria-rowindex={v.index + 1} style={{ position: "absolute", top: 0, left: 0, right: 0, height: ROW_H, transform: `translateY(${v.start}px)` }}>
                        <WaterfallRow
                          row={row}
                          totalMs={total}
                          selected={row.span.span_id === selectedId}
                          sentryCount={errorsBySpan.get(row.span.span_id)?.length ?? 0}
                          collapsed={collapsed.has(row.span.span_id)}
                          onSelect={() => setSpanParam(row.span.span_id, true)}
                          onToggle={() => toggle(row.span.span_id)}
                        />
                      </div>
                    );
                  })}
                </div>
              </div>
            </div>
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel defaultSize="38%" minSize="20%">
            <div ref={panelRef} className="h-full overflow-auto">
              {selected ? (
                <SpanPanel
                  span={selected.span}
                  offsetMs={selected.offsetMs}
                  traceStartMs={full.startMs}
                  errors={errorsBySpan.get(selected.span.span_id) ?? []}
                  logs={logsBySpan.get(selected.span.span_id) ?? []}
                  onOpenError={toErrors}
                />
              ) : (
                <p className="px-4 py-3 text-kumo-subtle">No span selected</p>
              )}
            </div>
          </ResizablePanel>
        </ResizablePanelGroup>
        </LayerCard>
      )}
    </Page>
  );
}
