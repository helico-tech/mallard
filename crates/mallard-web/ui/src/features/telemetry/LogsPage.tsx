import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { Button, Link as KumoLink, useKumoToastManager } from "@cloudflare/kumo";
import { ArrowDownIcon, PauseIcon, PlayIcon, XIcon } from "@phosphor-icons/react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useNavigate } from "react-router-dom";
import { otel } from "@/api/otel";
import type { LogRecord } from "@/api/types";
import { REFRESH_EVENT, useKeys } from "@/app/keys";
import { useInterval, useSettings } from "@/app/settings";
import { useParam, useWindow } from "@/app/window";
import { Legend, StackedBars } from "@/components/charts";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import { useNarrow, Severity, Attributes, CopyId, LIST_HEADER, EmptyState, ErrorState, Facts, Page, SearchField, Section, Service, StaleNote, TableSkeleton, toneBg } from "@/components/ui-bits";
import { severityName, severityTone, windowSeconds } from "@/lib/domain";
import { compact, dayTime, timeMs, timestamp, timestampFull } from "@/lib/format";
import { POLL_LIMIT, Tail } from "@/lib/logtail";
import { cycleLevel, parseLogFilter, type LogFilter } from "@/lib/query";
import { cn } from "@/lib/utils";

const MAX_RECORDS = 5000;
const ROW_H = 24;

type Item = { kind: "log"; id: string; r: LogRecord; truncatedBody: boolean } | { kind: "gap"; id: string };

interface State {
  items: Item[];
  loaded: boolean;
  error: unknown;
}

type Action = { type: "ok" } | { type: "reset" } | { type: "add"; items: Item[] } | { type: "error"; error: unknown } | { type: "loaded" };

function reducer(s: State, a: Action): State {
  switch (a.type) {
    case "ok":
      return s.error ? { ...s, error: null } : s;
    case "reset":
      return { items: [], loaded: false, error: null };
    case "loaded":
      return { ...s, loaded: true, error: null };
    case "error":
      return { ...s, error: a.error };
    case "add": {
      const next = s.items.concat(a.items);
      return { ...s, items: next.length > MAX_RECORDS ? next.slice(next.length - MAX_RECORDS) : next, error: null };
    }
  }
}

let nextId = 0;
const toItem = (r: LogRecord): Item => ({ kind: "log", id: `l${nextId++}`, r, truncatedBody: r.body.length > 400 });

function useLogStream(w: string, filter: LogFilter, filterKey: string, following: boolean) {
  const [reload, bump] = useState(0);
  useEffect(() => {
    const onRefresh = () => bump((n) => n + 1);
    window.addEventListener(REFRESH_EVENT, onRefresh);
    return () => window.removeEventListener(REFRESH_EVENT, onRefresh);
  }, []);
  const [state, dispatch] = useReducer(reducer, { items: [], loaded: false, error: null });
  const tail = useRef<Tail<LogRecord> | null>(null);
  const q = useMemo(() => ({
    service: filter.service,
    min_severity: filter.minSeverity,
    q: filter.q,
    trace_id: filter.trace,
    attributes: filter.attributes.join(",") || undefined,
  }), [filter]);

  useEffect(() => {
    const abort = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    dispatch({ type: "reset" });
    tail.current = null;
    const load = async () => {
      const start = new Date();
      try {
        const page = await otel.logs({ ...q, since: w, limit: 1000, order: "desc" }, abort.signal);
        const t = new Tail<LogRecord>(start);
        t.markSeen(page);
        tail.current = t;
        dispatch({ type: "add", items: [...page].reverse().map(toItem) });
        dispatch({ type: "loaded" });
      } catch (e) {
        if ((e as Error).name === "AbortError") return;
        dispatch({ type: "error", error: e });
        timer = setTimeout(() => void load(), 3000);
      }
    };
    void load();
    return () => {
      abort.abort();
      if (timer) clearTimeout(timer);
    };
  }, [q, w, filterKey, reload]);

  const poll = useCallback(async (signal: AbortSignal) => {
    const t = tail.current;
    if (!t) return;
    const started = Date.now();
    const { from, since, capped } = t.window(started);
    try {
      const page = await otel.logs({ ...q, from, since, order: "desc", limit: POLL_LIMIT }, signal);
      const { fresh, truncated } = t.accept(page, POLL_LIMIT, started, capped);
      const items: Item[] = fresh.map(toItem);
      if (truncated) items.unshift({ kind: "gap", id: `g${nextId++}` });
      if (items.length) dispatch({ type: "add", items });
      else dispatch({ type: "ok" });
    } catch (e) {
      if ((e as Error).name !== "AbortError") dispatch({ type: "error", error: e });
    }
  }, [q]);

  useEffect(() => {
    if (!following || !state.loaded) return;
    const abort = new AbortController();
    const id = setInterval(() => void poll(abort.signal), 2000);
    return () => {
      clearInterval(id);
      abort.abort();
    };
  }, [following, state.loaded, poll]);

  return state;
}

function LogRow({ item, selected, onSelect, longWindow }: { item: Item; selected: boolean; onSelect: () => void; longWindow: boolean }) {
  if (item.kind === "gap") {
    return <div className="flex h-full items-center justify-center bg-kumo-recessed text-xs text-kumo-subtle">Older records skipped (paused or too many to show)</div>;
  }
  const { r } = item;
  const name = severityName(r.severity_number, r.severity_text);
  const tone = severityTone(name);
  return (
    <div
      role="row"
      aria-selected={selected}
      onClick={onSelect}
      className={cn("grid h-full cursor-pointer grid-cols-[8px_88px_48px_minmax(0,1fr)] @3xl:grid-cols-[8px_104px_48px_minmax(60px,140px)_minmax(0,1fr)] items-baseline gap-2 pr-4 leading-6 whitespace-nowrap", selected ? "row-selected" : "hover:bg-kumo-tint")}
    >
      <span aria-hidden className={cn("h-6 w-[3px] self-start", toneBg(tone))} />
      <span className="tnum text-kumo-subtle">{longWindow ? dayTime(r.time) : timeMs(r.time)}</span>
      <Severity name={name} />
      <Service name={r.service_name} className="hidden text-kumo-subtle @3xl:inline-flex" />
      <span className={cn("mono truncate", r.severity_number >= 17 ? "text-kumo-danger" : "text-kumo-default")}>
        {r.trace_id && <span aria-label="has a trace" className="mr-1 text-kumo-subtle">◆</span>}
        {r.body.replaceAll("\n", " ").slice(0, 400)}
      </span>
    </div>
  );
}

function RecordPanel({ r, onClose }: { r: LogRecord; onClose: () => void }) {
  const name = severityName(r.severity_number, r.severity_text);
  return (
    <div className="flex h-full flex-col gap-4 overflow-auto px-4 py-3" aria-label="Log record">
      <div className="flex items-start justify-between gap-2">
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-3">
          <Severity name={name} />
          <Service name={r.service_name} />
          <span className="tnum text-kumo-subtle">{timestampFull(r.time)}</span>
        </div>
        <Button size="sm" variant="ghost" shape="square" aria-label="Close (Esc)" icon={<XIcon />} onClick={onClose} />
      </div>
      <pre className="mono overflow-auto max-h-72 shrink-0 rounded-md bg-kumo-recessed px-3 py-2 [overflow-wrap:anywhere] whitespace-pre-wrap">{r.body}</pre>
      <Facts rows={[
        ["Trace", r.trace_id ? <span className="inline-flex flex-wrap items-center gap-2"><KumoLink className="mono" href={`/telemetry/traces/${r.trace_id}?hint=${encodeURIComponent(r.time)}`}>{r.trace_id}</KumoLink></span> : "–"],
        ["Span", r.span_id ? <CopyId value={r.span_id} /> : "–"],
        ["Severity", `${r.severity_text || name} (${r.severity_number})`],
        ["Observed", <span className="tnum">{timestampFull(r.observed_time)}</span>],
        ["Event", r.event_name || "–"],
        ["Scope", r.scope_name || "–"],
      ]} />
      <Attributes title="Attributes" attrs={r.attributes} />
      <Attributes title="Resource" attrs={r.resource_attributes} />
    </div>
  );
}

export function LogsPage() {
  const { w } = useWindow("telemetry");
  const [f, setF] = useParam("f");
  const navigate = useNavigate();
  const { paused } = useSettings();
  const toasts = useKumoToastManager();
  const refetchInterval = useInterval(5000);
  const narrow = useNarrow();
  const [follow, setFollow] = useState(true);
  const [atEnd, setAtEnd] = useState(true);
  // The row cursor; null means the last row, so it follows new records.
  const [cursorId, setCursorId] = useState<string | null>(null);
  const [panelOpen, setPanelOpen] = useState(false);

  const parsed = parseLogFilter(f);
  const filter: LogFilter = useMemo(() => ("error" in parsed ? { attributes: [] } : parsed), [f]); // eslint-disable-line react-hooks/exhaustive-deps
  const following = follow && !paused;
  const state = useLogStream(w, filter, f, following);
  const items = state.items;

  // The filter or window changing starts over and follows again, as in the TUI.
  useEffect(() => {
    setFollow(true);
    setCursorId(null);
    setPanelOpen(false);
    setAtEnd(true);
  }, [f, w]);

  const volume = useQuery({
    queryKey: ["otel", "log-volume", w, filter.service, filter.q],
    queryFn: ({ signal }) => otel.logSeries({ since: w, service: filter.service, q: filter.q }, signal),
    refetchInterval,
    placeholderData: keepPreviousData,
  });
  const volumeFiltered = !!(filter.minSeverity || filter.trace || filter.attributes.length);
  const pts = volume.data?.points ?? [];
  const totals = pts.reduce((a, p) => ({ error: a.error + p.error, warn: a.warn + p.warn, info: a.info + p.info, debug: a.debug + p.debug }), { error: 0, warn: 0, info: 0, debug: 0 });

  const scrollRef = useRef<HTMLDivElement>(null);
  const virt = useVirtualizer({ count: items.length, getScrollElement: () => scrollRef.current, estimateSize: () => ROW_H, overscan: 20 });
  const scrollToEnd = useCallback(() => {
    if (items.length) virt.scrollToIndex(items.length - 1, { align: "end" });
  }, [items.length, virt]);

  useEffect(() => {
    if (atEnd && following) scrollToEnd();
  }, [items.length, atEnd, following, scrollToEnd]);

  const found = cursorId === null ? -1 : items.findIndex((i) => i.id === cursorId);
  const index = found >= 0 ? found : items.length - 1;
  const selected = index >= 0 ? items[index] : undefined;
  const selectedId = selected?.id ?? null;
  const selectedRecord = selected?.kind === "log" ? selected.r : undefined;
  const openRecord = panelOpen ? selectedRecord : undefined;

  const go = (i: number) => {
    if (!items.length) return;
    const n = Math.max(0, Math.min(items.length - 1, i));
    setCursorId(n === items.length - 1 ? null : items[n]!.id);
    setAtEnd(n === items.length - 1);
    virt.scrollToIndex(n, { align: "auto" });
  };
  const toggleFollow = () => {
    const next = !follow;
    setFollow(next);
    toasts.add({ title: next ? "Following new logs" : "Paused" });
    if (next) {
      setCursorId(null);
      setAtEnd(true);
      scrollToEnd();
    }
  };
  const openTrace = () => {
    if (!selectedRecord) return;
    if (selectedRecord.trace_id) navigate(`/telemetry/traces/${selectedRecord.trace_id}?hint=${encodeURIComponent(selectedRecord.time)}`);
    else toasts.add({ title: "This record has no trace id" });
  };

  useKeys({
    j: () => go(index + 1),
    k: () => go(index - 1),
    ArrowDown: () => go(index + 1),
    ArrowUp: () => go(index - 1),
    PageDown: () => go(index + 20),
    PageUp: () => go(index - 20),
    Enter: () => selectedRecord && setPanelOpen((v) => !v),
    q: () => setPanelOpen(false),
    g: () => go(0),
    Home: () => go(0),
    G: () => go(items.length - 1),
    End: () => go(items.length - 1),
    s: () => {
      const next = cycleLevel(f);
      setF(next);
      const n = /(?:^|\s)level=(\w+)/.exec(next)?.[1];
      toasts.add({ title: n === "info" ? "Info and above" : n === "warn" ? "Warnings and errors" : n === "error" ? "Errors only" : "All severities" });
    },
    " ": toggleFollow,
    F: toggleFollow,
    o: openTrace,
    c: () => setF(""),
    Escape: () => {
      if (openRecord) setPanelOpen(false);
      else if (f) setF("");
    },
  });

  const list = (
    <div className="flex h-full min-h-0 flex-col">
      <div className={cn(LIST_HEADER, "grid-cols-[8px_88px_48px_minmax(0,1fr)] @3xl:grid-cols-[8px_104px_48px_minmax(60px,140px)_minmax(0,1fr)] gap-2 pr-4")} aria-hidden>
        <span />
        <span>Time</span>
        <span>Level</span>
        <span className="hidden @3xl:inline">Service</span>
        <span>Message</span>
      </div>
      <div className="relative min-h-0 flex-1">
        <div
          ref={scrollRef}
          role="log"
          aria-label="Log records, oldest first"
          aria-rowcount={items.length}
          tabIndex={0}
          className="h-full overflow-auto"
          onScroll={(e) => {
            const el = e.currentTarget;
            setAtEnd(el.scrollHeight - el.scrollTop - el.clientHeight < ROW_H * 2);
          }}
        >
          <div style={{ height: virt.getTotalSize(), position: "relative" }}>
            {virt.getVirtualItems().map((v) => {
              const item = items[v.index]!;
              return (
                <div key={item.id} data-row-id={item.id} aria-rowindex={v.index + 1} style={{ position: "absolute", top: 0, left: 0, right: 0, height: ROW_H, transform: `translateY(${v.start}px)` }}>
                  <LogRow item={item} selected={item.id === selectedId} onSelect={() => { setCursorId(item.id); setPanelOpen(item.kind === "log"); }} longWindow={windowSeconds(w) > 86400} />
                </div>
              );
            })}
          </div>
        </div>
        {!atEnd && items.length > 0 && (
          <div className="absolute right-4 bottom-3">
            <Button variant="secondary" icon={<ArrowDownIcon />} onClick={() => { setAtEnd(true); scrollToEnd(); }}>Jump to latest</Button>
          </div>
        )}
      </div>
    </div>
  );

  return (
    <Page>
      <div className="flex shrink-0 flex-wrap items-start gap-2">
        <SearchField
          value={f}
          onApply={setF}
          mono
          label="Filter logs"
          placeholder="service=api level=warn trace=abc order placed key=value"
          validate={(v) => {
            const p = parseLogFilter(v);
            return "error" in p ? p.error : null;
          }}
        />
        {f && <Button variant="ghost" icon={<XIcon />} onClick={() => setF("")}>Clear</Button>}
        <Button variant={following ? "secondary" : "primary"} aria-pressed={following} onClick={toggleFollow} icon={following ? <PauseIcon /> : <PlayIcon />}>
          {following ? "Pause" : "Follow"}
        </Button>
      </div>
      <Section
        title={<>Volume{volumeFiltered ? <span className="ml-2 text-xs font-normal text-kumo-subtle">service and text filter only</span> : null}</>}
        right={<Legend items={[
          { label: "Error", color: "var(--chart-attention)", value: compact(totals.error) },
          { label: "Warn", color: "var(--chart-warning)", value: compact(totals.warn) },
          { label: "Info", color: "var(--series-1)", value: compact(totals.info) },
          { label: "Debug", color: "var(--chart-neutral)", value: compact(totals.debug) },
        ]} />}
      >
        <div className="px-4 py-3">
        {volume.data && pts.length === 0 ? (
          <p className="py-3 text-center text-xs text-kumo-subtle">No logs in the last {w}</p>
        ) : (
          <StackedBars
            height={44}
            axis={[`-${w}`, "now"]}
            buckets={pts.map((p) => ({ time: Date.parse(p.time), values: { error: p.error, warn: p.warn, info: p.info, debug: p.debug } }))}
            series={[
              { key: "error", label: "Error", color: "var(--chart-attention)" },
              { key: "warn", label: "Warn", color: "var(--chart-warning)" },
              { key: "info", label: "Info", color: "var(--series-1)" },
              { key: "debug", label: "Debug", color: "var(--chart-neutral)" },
            ]}
            ariaLabel={`Log volume in the last ${w}: ${compact(totals.error)} errors, ${compact(totals.warn)} warnings`}
            stepLabel={(t) => `${timestamp(new Date(t))} UTC`}
          />
        )}
        </div>
      </Section>
      <Section
        className="min-h-0 flex-1 shrink!"
        title="Records"
        right={
          <span role="status" className="flex items-center gap-2">
            <span className={following ? "text-kumo-success" : "text-kumo-warning"}>{following ? "Following" : "Paused"}</span>
            <span>{compact(items.filter((i) => i.kind === "log").length)} records{items.length >= MAX_RECORDS ? ", oldest dropped" : ""}</span>
          </span>
        }
      >
        {!!state.error && items.length > 0 && <StaleNote error={state.error} />}
        {!!state.error && items.length === 0 ? (
          <ErrorState error={state.error} title="Could not load logs" />
        ) : !state.loaded ? (
          <TableSkeleton />
        ) : items.length === 0 ? (
          <EmptyState
            title="No logs match"
            description={following ? "Waiting for new records." : "Follow, change the filter, or widen the time window."}
            action={f ? <Button onClick={() => setF("")}>Clear the filter</Button> : undefined}
          />
        ) : openRecord ? (
          <ResizablePanelGroup key={narrow ? "v" : "h"} orientation={narrow ? "vertical" : "horizontal"} className="min-h-0 flex-1">
            <ResizablePanel defaultSize="62%" minSize="30%">{list}</ResizablePanel>
            <ResizableHandle />
            <ResizablePanel defaultSize="38%" minSize="20%"><RecordPanel r={openRecord} onClose={() => setPanelOpen(false)} /></ResizablePanel>
          </ResizablePanelGroup>
        ) : (
          <div className="min-h-0 flex-1">{list}</div>
        )}
      </Section>
    </Page>
  );
}

