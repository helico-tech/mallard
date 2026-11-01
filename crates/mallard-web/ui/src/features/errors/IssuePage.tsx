import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, Link as KumoLink, Switch, Table, Tabs, Tooltip, useKumoToastManager } from "@cloudflare/kumo";
import { ArrowCounterClockwiseIcon, CaretLeftIcon, CaretRightIcon, CheckIcon, EyeSlashIcon, GraphIcon } from "@phosphor-icons/react";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError } from "@/api/client";
import { sentry } from "@/api/sentry";
import type { EventDetail, EventSummary, IssueDetail } from "@/api/types";
import { useKeys } from "@/app/keys";
import { useInterval } from "@/app/settings";
import { useParam, useWindow } from "@/app/window";
import { Sparkline } from "@/components/charts";
import { useListNav } from "@/components/list";
import { useBack, Ago, CopyId, DataTable, DetailHeader, EmptyState, ErrorState, Facts, KeyHint, Level, Pad, Page, Section, StaleNote, Status, TableSkeleton } from "@/components/ui-bits";
import { breadcrumbs, context, contexts, exceptions, framesNewestFirst, functionName, location, mechanismLabel, message, request, type Exception, type Frame } from "@/lib/event";
import { compact, relative, short, timeMs, timestamp } from "@/lib/format";
import { cn } from "@/lib/utils";
import { useSetStatus } from "./data";

const SECTIONS = ["overview", "stack", "breadcrumbs", "events", "json"] as const;
type SectionKey = (typeof SECTIONS)[number];

function Overview({ issue, event, w }: { issue: IssueDetail; event?: EventDetail; w: string }) {
  const ctx = event ? contexts(event.payload) : [];
  const req = event ? request(event.payload) : undefined;
  return (
    <div className="grid gap-4 2xl:grid-cols-[minmax(0,1fr)_minmax(320px,0.7fr)]">
      <div className="flex min-w-0 flex-col gap-4">
        <Section title="Details">
          <Pad>
            <Facts rows={[
              ["Status", <Status status={issue.status} />],
              ["Level", <Level level={issue.level} />],
              ["Platform", issue.platform],
              ["Events", compact(issue.count)],
              ["Users", compact(issue.user_count)],
              ["First seen", <>{timestamp(issue.first_seen)} <span className="text-kumo-subtle">({relative(issue.first_seen)})</span></>],
              ["Last seen", <>{timestamp(issue.last_seen)} <span className="text-kumo-subtle">({relative(issue.last_seen)})</span></>],
              ["Exception", issue.exception_type ?? "–"],
              ["First release", issue.first_release ?? "–"],
              ["Last release", issue.last_release ?? "–"],
            ]} />
          </Pad>
        </Section>
        <Section title={`Trend, last ${w}`}>
          <Pad><Sparkline fluid values={issue.trend} width={480} height={44} label={`Events in the last ${w}`} /></Pad>
        </Section>
        <Section title="Shown event">
          <Pad>
            {event ? (
              <Facts rows={[
                ["Event", <CopyId value={event.event_id} />],
                ["Time", `${timestamp(event.timestamp)} UTC`],
                ["Environment", event.environment ?? "–"],
                ["Release", event.release ?? "–"],
                ["User", event.user ?? "–"],
                ["Trace", event.trace_id ? <span className="inline-flex flex-wrap items-center gap-2"><KumoLink className="mono" href={`/telemetry/traces/${event.trace_id}?hint=${encodeURIComponent(event.timestamp)}`}>{event.trace_id}</KumoLink><KeyHint>x</KeyHint></span> : "–"],
                ...ctx.map(([k, v]): [string, React.ReactNode] => [k[0]!.toUpperCase() + k.slice(1), v]),
                ...(req ? ([["Request", <span className="mono">{req}</span>]] as [string, React.ReactNode][]) : []),
              ]} />
            ) : (
              <p className="text-kumo-subtle">Loading event…</p>
            )}
          </Pad>
        </Section>
        <Section title="Activity">
          {issue.activity.length === 0 ? (
            <Pad className="text-kumo-subtle">No status changes</Pad>
          ) : (
            <ul className="divide-y divide-kumo-hairline">
              {issue.activity.map((a) => (
                <li key={`${a.at}${a.status}`} className="flex flex-wrap items-center gap-x-3 px-4 py-2">
                  <span className="tnum text-kumo-subtle">{timestamp(a.at)}</span>
                  <Status status={a.status} />
                  <span className="min-w-0 break-words">{a.reason}</span>
                </li>
              ))}
            </ul>
          )}
        </Section>
      </div>
      <Section title="Tags" className="self-start">
        {issue.tags.length === 0 ? (
          <Pad className="text-kumo-subtle">No tags</Pad>
        ) : (
          <div className="flex flex-col gap-4 px-4 py-3">
            {issue.tags.map((t) => {
              const sum = t.top.reduce((a, v) => a + v.count, 0) || 1;
              return (
                <div key={t.key}>
                  <div className="mb-1 flex justify-between gap-2">
                    <span className="truncate font-medium">{t.key}</span>
                    <span className="tnum shrink-0 text-xs text-kumo-subtle">{compact(t.unique)} unique</span>
                  </div>
                  <ul className="space-y-0.5">
                    {t.top.map((v) => {
                      const pct = (v.count / sum) * 100;
                      return (
                        <li key={v.value} className="tnum grid grid-cols-[minmax(0,1fr)_72px_44px] items-center gap-2">
                          <span className="truncate" title={v.value}>{v.value}</span>
                          <span className="h-1.5 rounded-full bg-kumo-recessed" aria-hidden><span className="block h-1.5 rounded-full bg-kumo-brand" style={{ width: `${pct}%` }} /></span>
                          <span className="text-right text-xs text-kumo-subtle">{pct.toFixed(1)}%</span>
                        </li>
                      );
                    })}
                  </ul>
                </div>
              );
            })}
          </div>
        )}
      </Section>
    </div>
  );
}

function FrameRow({ frame, selected, onSelect, id }: { frame: Frame; selected: boolean; onSelect: () => void; id: string }) {
  const lines = context(frame);
  return (
    <li data-row-id={id} className={cn("border-b border-kumo-hairline last:border-b-0", selected && "row-selected")}>
      <button type="button" aria-expanded={selected} onClick={onSelect} className="flex w-full flex-wrap items-baseline gap-x-3 px-4 py-2 text-left hover:bg-kumo-tint">
        <span className={cn("mono min-w-0 [overflow-wrap:anywhere]", frame.inApp ? "font-medium text-kumo-default" : "text-kumo-subtle")}>{functionName(frame)}</span>
        <span className="mono min-w-0 [overflow-wrap:anywhere] text-kumo-subtle" dir="ltr" title={location(frame)}>{location(frame)}</span>
        {frame.inApp && <Badge variant="neutral">In app</Badge>}
      </button>
      {selected && (
        <div className="overflow-x-auto bg-kumo-recessed py-2">
          {lines.length === 0 ? (
            <p className="px-4 text-kumo-subtle">No source context for this frame</p>
          ) : (
            <pre className="mono w-max min-w-full">
              {lines.map((l, i) => (
                <div key={i} className={cn("flex", l.error && "bg-[color-mix(in_oklab,var(--chart-attention)_16%,transparent)] font-medium")}>
                  <span className="w-14 shrink-0 pr-3 text-right text-kumo-subtle select-none">{l.line ?? ""}</span>
                  <code className="pr-4 whitespace-pre">{l.text}</code>
                </div>
              ))}
            </pre>
          )}
        </div>
      )}
    </li>
  );
}

function StackTrace({ event, loading, inAppOnly, setInAppOnly, raw, setRaw }: {
  event?: EventDetail;
  loading: boolean;
  inAppOnly: boolean;
  setInAppOnly: (v: boolean) => void;
  raw: boolean;
  setRaw: (v: boolean) => void;
}) {
  const list = useMemo(() => (event ? exceptions(event.payload) : []), [event]);
  const hasRaw = list.some((e) => e.rawFrames?.length);

  const groups = useMemo(() => list.map((e) => {
    const all = framesNewestFirst(e, raw && hasRaw);
    return { e, frames: inAppOnly ? all.filter((f) => f.inApp) : all };
  }), [list, raw, hasRaw, inAppOnly]);
  const flat = useMemo(() => groups.flatMap((g, gi) => g.frames.map((f, fi) => ({ f, id: `f${gi}-${fi}` }))), [groups]);
  const firstInApp = flat.find((x) => x.f.inApp)?.id;

  const nav = useListNav({
    listKey: `frames:${event?.event_id ?? ""}`,
    items: flat,
    idOf: (x) => x.id,
  });
  const { select } = nav;
  useEffect(() => {
    select(firstInApp ?? flat[0]?.id ?? null);
    // The frames change with these two; so does what is under the cursor.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [firstInApp, select, inAppOnly, raw]);

  if (!event) return loading ? <TableSkeleton /> : null;
  if (list.length === 0) {
    const m = message(event.payload);
    return <EmptyState title="This event has no stack trace" description={m ?? undefined} />;
  }
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-kumo-subtle">
        <span>Newest first</span>
        <Switch  label="In-app only (a)" checked={inAppOnly} onCheckedChange={setInAppOnly} />
        {hasRaw && <Switch  label="Raw frames (w)" checked={raw} onCheckedChange={setRaw} />}
      </div>
      {groups.map(({ e, frames }, gi) => (
        <ExceptionBlock key={gi} e={e} frames={frames} gi={gi} chained={gi > 0} selectedId={nav.selectedId} onSelect={select} inAppOnly={inAppOnly} />
      ))}
    </div>
  );
}

function ExceptionBlock({ e, frames, gi, chained, selectedId, onSelect, inAppOnly }: {
  e: Exception; frames: Frame[]; gi: number; chained: boolean; selectedId: string | null; onSelect: (id: string) => void; inAppOnly: boolean;
}) {
  const mech = mechanismLabel(e);
  return (
    <div>
      {chained && <p className="mb-1.5 text-kumo-subtle">Caused by</p>}
      <Section>
        <header className="flex flex-col gap-0.5 border-b border-kumo-hairline px-4 py-3">
          <p className="font-medium [overflow-wrap:anywhere] text-kumo-danger">{e.type}{e.value ? `: ${e.value}` : ""}</p>
          {mech && <p className="text-xs text-kumo-subtle">{mech}</p>}
        </header>
        {frames.length === 0 ? (
          <p className="px-4 py-3 text-kumo-subtle">{inAppOnly ? "No in-app frames. Press a to show all frames." : "No stack frames"}</p>
        ) : (
          <ul>
            {frames.map((f, fi) => {
              const id = `f${gi}-${fi}`;
              return <FrameRow key={id} id={id} frame={f} selected={selectedId === id} onSelect={() => onSelect(id)} />;
            })}
          </ul>
        )}
      </Section>
    </div>
  );
}

function Breadcrumbs({ event }: { event?: EventDetail }) {
  const rows = useMemo(() => (event ? breadcrumbs(event.payload) : []), [event]);
  const items = useMemo(() => rows.map((c, i) => ({ c, id: String(i) })), [rows]);
  const { selectedId, select } = useListNav({ listKey: `crumbs:${event?.event_id ?? ""}`, items, idOf: (x) => x.id });
  if (!event) return <TableSkeleton />;
  if (!rows.length) return <EmptyState title="This event has no breadcrumbs" />;
  return (
    <Section className="min-h-0 flex-1 shrink!">
        <DataTable fixed>
          <caption className="sr-only">Breadcrumbs, newest first</caption>
          <Table.Header variant="compact" sticky><Table.Row><Table.Head className="w-28">Time</Table.Head><Table.Head className="hidden w-24 @lg:table-cell">Level</Table.Head><Table.Head className="hidden w-32 @lg:table-cell">Category</Table.Head><Table.Head>Message</Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {items.map(({ c, id }) => (
              <Table.Row key={id} data-row-id={id} onClick={() => select(id)} variant={selectedId === id ? "selected" : "default"}>
                <Table.Cell className="tnum text-kumo-subtle">{c.timestamp ? timeMs(c.timestamp) : "–"}</Table.Cell>
                <Table.Cell className="hidden @lg:table-cell"><Level level={c.level} /></Table.Cell>
                <Table.Cell className="hidden truncate text-kumo-subtle @lg:table-cell" title={c.category}>{c.category ?? "–"}</Table.Cell>
                <Table.Cell className="mono [overflow-wrap:anywhere]">{c.text}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </DataTable>
      </Section>
  );
}

function EventsList({ events, shownId, onShow, hasMore, loadMore, loadingMore }: {
  events: EventSummary[]; shownId?: string; onShow: (id: string) => void; hasMore: boolean; loadMore: () => void; loadingMore: boolean;
}) {
  const { selectedId, select, index } = useListNav({
    listKey: "events",
    items: events,
    idOf: (e) => e.event_id,
    onOpen: (e) => onShow(e.event_id),
  });
  useEffect(() => {
    if (hasMore && !loadingMore && events.length > 0 && index === events.length - 1) loadMore();
  }, [hasMore, loadingMore, index, events.length, loadMore]);
  if (!events.length) return <EmptyState title="No events in this issue" />;
  return (
      <Section className="min-h-0 flex-1 shrink!">
        <DataTable fixed>
          <caption className="sr-only">Events of this issue, newest first. Enter shows an event.</caption>
          <Table.Header variant="compact" sticky><Table.Row><Table.Head className="w-9"><span className="sr-only">Shown</span></Table.Head><Table.Head className="w-24">When</Table.Head><Table.Head className="w-28">Event</Table.Head><Table.Head className="hidden w-28 @3xl:table-cell">Env</Table.Head><Table.Head className="hidden w-28 @4xl:table-cell">Release</Table.Head><Table.Head className="hidden w-32 @2xl:table-cell">User</Table.Head><Table.Head>Message</Table.Head></Table.Row></Table.Header>
          <Table.Body>
            {events.map((e) => (
              <Table.Row key={e.event_id} data-row-id={e.event_id} onClick={() => select(e.event_id)} onDoubleClick={() => onShow(e.event_id)} variant={selectedId === e.event_id ? "selected" : "default"}>
                <Table.Cell aria-label={e.event_id === shownId ? "Shown" : undefined}>{e.event_id === shownId && <span aria-hidden className="block size-2 rounded-full bg-kumo-brand" />}</Table.Cell>
                <Table.Cell><Ago t={e.timestamp} /></Table.Cell>
                <Table.Cell><button type="button" onClick={() => onShow(e.event_id)} className="mono text-kumo-link hover:underline">{short(e.event_id, 10)}</button></Table.Cell>
                <Table.Cell className="hidden truncate text-kumo-subtle @3xl:table-cell" title={e.environment ?? undefined}>{e.environment ?? "–"}</Table.Cell>
                <Table.Cell className="hidden truncate text-kumo-subtle @4xl:table-cell" title={e.release ?? undefined}>{e.release ?? "–"}</Table.Cell>
                <Table.Cell className="hidden truncate text-kumo-subtle @2xl:table-cell" title={e.user ?? undefined}>{e.user ?? "–"}</Table.Cell>
                <Table.Cell className="truncate" title={e.message ?? e.title}>{e.message ?? e.title}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </DataTable>
        <div className="flex justify-center px-4 py-3">
          {hasMore ? <Button variant="secondary" loading={loadingMore} onClick={loadMore}>Load older events</Button> : <p className="text-xs text-kumo-subtle">End of the list</p>}
        </div>
      </Section>
  );
}

/** Token colours like the TUI's JSON view; every string is already cleaned. */
function JsonLine({ line }: { line: string }) {
  const parts: React.ReactNode[] = [];
  const re = /("(?:[^"\\]|\\.)*")(\s*:)?|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) {
    if (m.index > last) parts.push(line.slice(last, m.index));
    if (m[1]) {
      parts.push(<span key={m.index} className={m[2] ? "text-kumo-default" : "text-kumo-success"}>{m[1]}</span>);
      if (m[2]) parts.push(m[2]);
    } else if (m[3]) parts.push(<span key={m.index} className="text-kumo-info">{m[3]}</span>);
    else parts.push(<span key={m.index} className="text-kumo-warning">{m[4]}</span>);
    last = m.index + m[0].length;
  }
  parts.push(line.slice(last));
  return <>{parts}</>;
}

const JSON_LINE_H = 20;
/** A longer line is cut on screen; the copy in the event is whole. */
const JSON_LINE_MAX = 20_000;

function JsonView({ event, scrollRef }: { event?: EventDetail; scrollRef: React.RefObject<HTMLDivElement | null> }) {
  const lines = useMemo(() => (event ? JSON.stringify(event.payload, null, 2).split("\n") : []), [event]);
  const longest = useMemo(() => lines.reduce((a, l) => Math.max(a, Math.min(l.length, JSON_LINE_MAX)), 0), [lines]);
  const virt = useVirtualizer({ count: lines.length, getScrollElement: () => scrollRef.current, estimateSize: () => JSON_LINE_H, overscan: 30 });
  if (!event) return <TableSkeleton />;
  return (
    <Section className="min-h-0 flex-1 shrink!">
      <div ref={scrollRef} className="mono min-h-0 flex-1 overflow-auto" role="region" aria-label="Event as JSON" tabIndex={0}>
        <div style={{ height: virt.getTotalSize(), minWidth: `calc(${longest}ch + 4rem)`, position: "relative" }}>
          {virt.getVirtualItems().map((v) => {
            const line = lines[v.index]!;
            return (
              <div key={v.index} className="flex whitespace-pre" style={{ position: "absolute", top: 0, left: 0, right: 0, height: JSON_LINE_H, lineHeight: `${JSON_LINE_H}px`, transform: `translateY(${v.start}px)` }}>
                <span className="w-12 shrink-0 pr-3 text-right text-kumo-subtle select-none">{v.index + 1}</span>
                <code><JsonLine line={line.length > JSON_LINE_MAX ? `${line.slice(0, JSON_LINE_MAX)}…` : line} /></code>
              </div>
            );
          })}
        </div>
      </div>
    </Section>
  );
}

export function IssuePage() {
  const { project = "", issueId = "" } = useParams();
  const navigate = useNavigate();
  const { w } = useWindow("errors");
  const [section, setSection] = useParam("section", "overview");
  const [eventParam, setEventParam] = useParam("event");
  const [, setParams] = useSearchParams();
  const [inAppOnly, setInAppOnly] = useState(false);
  const [raw, setRaw] = useState(false);
  const outerScroller = useRef<HTMLDivElement>(null);
  const jsonScroller = useRef<HTMLDivElement>(null);
  const scroller = section === "json" ? jsonScroller : outerScroller;
  const refetchInterval = useInterval(5000);
  const setStatus = useSetStatus(project);
  const toasts = useKumoToastManager();

  const issueQ = useQuery({
    queryKey: ["sentry", "issue", project, issueId, w],
    queryFn: ({ signal }) => sentry.issue(project, issueId, w, signal),
    refetchInterval,
    placeholderData: (prev) => prev,
  });
  const eventsQ = useInfiniteQuery({
    queryKey: ["sentry", "issue-events", project, issueId],
    queryFn: ({ signal, pageParam }) => sentry.events(project, issueId, pageParam, signal),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
  const events = useMemo(() => eventsQ.data?.pages.flatMap((p) => p.events) ?? [], [eventsQ.data]);
  // The head of the list, so "older" and "newer" start from the newest event.
  const shownId = eventParam || events[0]?.event_id || issueQ.data?.latest_event_id || undefined;
  const eventQ = useQuery({
    queryKey: ["sentry", "event", project, shownId, issueId],
    queryFn: ({ signal }) => sentry.event(project, shownId!, issueId, signal),
    enabled: !!shownId,
    placeholderData: (prev) => prev,
  });
  const event = eventQ.data;
  const index = events.findIndex((e) => e.event_id === shownId);

  const step = async (dir: 1 | -1) => {
    if (dir === -1) {
      if (index <= 0) return toasts.add({ title: "Already at the newest event" });
      return setEventParam(events[index - 1]!.event_id, true);
    }
    const next = events[index + 1];
    if (next) return setEventParam(next.event_id, true);
    if (eventsQ.hasNextPage) {
      const res = await eventsQ.fetchNextPage();
      const more = res.data?.pages.flatMap((p) => p.events)[index + 1];
      if (more) return setEventParam(more.event_id, true);
    }
    toasts.add({ title: "Already at the oldest event" });
  };

  const { fetchNextPage } = eventsQ;
  const loadOlder = useCallback(() => void fetchNextPage(), [fetchNextPage]);
  const back = useBack(`/errors/${encodeURIComponent(project)}?w=${w}`);
  const issue = issueQ.data;
  const write = (status: "resolved" | "ignored" | "unresolved") => {
    if (!issue) return;
    if (issue.status === status) toasts.add({ title: `Already ${status}` });
    else setStatus.mutate({ id: issue.id, title: issue.title, status });
  };
  const jump = () => {
    if (event?.trace_id) navigate(`/telemetry/traces/${event.trace_id}?hint=${encodeURIComponent(event.timestamp)}`);
    else toasts.add({ title: "This event has no trace id" });
  };
  const show = (id: string) => {
    setParams(
      (p) => {
        p.set("event", id);
        p.set("section", "stack");
        return p;
      },
      { replace: true },
    );
  };

  useKeys({
    Escape: back,
    q: back,
    Backspace: back,
    x: jump,
    "[": () => void step(1),
    "]": () => void step(-1),
    R: () => write("resolved"),
    I: () => write("ignored"),
    U: () => write("unresolved"),
    a: () => setInAppOnly((v) => !v),
    w: () => {
      if (event && exceptions(event.payload).some((e) => e.rawFrames?.length)) setRaw((v) => !v);
      else toasts.add({ title: "This event has no raw (unsymbolicated) frames" });
    },
    ArrowRight: () => setSection(SECTIONS[(SECTIONS.indexOf(section as SectionKey) + 1) % SECTIONS.length]!, true),
    ArrowLeft: () => setSection(SECTIONS[(SECTIONS.indexOf(section as SectionKey) - 1 + SECTIONS.length) % SECTIONS.length]!, true),
    // Overview and JSON have no rows: the keys scroll, as in the TUI.
    ...(section === "overview" || section === "json"
      ? {
          j: () => scroller.current?.scrollBy({ top: 40 }),
          ArrowDown: () => scroller.current?.scrollBy({ top: 40 }),
          k: () => scroller.current?.scrollBy({ top: -40 }),
          ArrowUp: () => scroller.current?.scrollBy({ top: -40 }),
          PageDown: () => scroller.current?.scrollBy({ top: 400 }),
          " ": () => scroller.current?.scrollBy({ top: 400 }),
          PageUp: () => scroller.current?.scrollBy({ top: -400 }),
          g: () => scroller.current?.scrollTo({ top: 0 }),
          Home: () => scroller.current?.scrollTo({ top: 0 }),
          G: () => scroller.current?.scrollTo({ top: scroller.current.scrollHeight }),
          End: () => scroller.current?.scrollTo({ top: scroller.current.scrollHeight }),
        }
      : {}),
    l: () => setSection(SECTIONS[(SECTIONS.indexOf(section as SectionKey) + 1) % SECTIONS.length]!, true),
    h: () => setSection(SECTIONS[(SECTIONS.indexOf(section as SectionKey) - 1 + SECTIONS.length) % SECTIONS.length]!, true),
    ...Object.fromEntries(SECTIONS.map((s, i) => [String(i + 1), () => setSection(s, true)])),
  });

  if (issueQ.isError && !issue) {
    return (
      <Page>
        {issueQ.error instanceof ApiError && issueQ.error.kind === "not_found" ? (
          <EmptyState title="This issue does not exist" description="Check the id, or look in another project." action={<Button onClick={back}>Go back</Button>} />
        ) : (
          <ErrorState error={issueQ.error} onRetry={() => void issueQ.refetch()} title="Could not load this issue" />
        )}
      </Page>
    );
  }
  if (!issue) return <TableSkeleton />;

  const crumbCount = event ? breadcrumbs(event.payload).length : undefined;
  const label = (name: string, n?: number) => (n === undefined ? name : `${name} ${compact(n)}`);

  return (
    <Page>
      <DetailHeader
        title={issue.title}
        meta={
          <>
            <Level level={issue.level} />
            <Status status={issue.status} />
            {issue.culprit && <span className="mono truncate" title={issue.culprit}>{issue.culprit}</span>}
            <span>{compact(issue.count)} events</span>
            <span>{compact(issue.user_count)} users</span>
            <span>last seen <Ago t={issue.last_seen} /></span>
            <span>first seen <Ago t={issue.first_seen} /></span>
            <span>in the last {w}</span>
          </>
        }
        actions={
          <>
            {event?.trace_id && (
              <Tooltip content="Open the trace (x)" asChild>
                <Button variant="secondary" icon={<GraphIcon />} onClick={jump}>Trace</Button>
              </Tooltip>
            )}
            {issue.status === "unresolved" ? (
              <>
                <Tooltip content="Ignore (I)" asChild><Button variant="secondary" icon={<EyeSlashIcon />} onClick={() => write("ignored")}>Ignore</Button></Tooltip>
                <Tooltip content="Resolve (R)" asChild><Button variant="primary" icon={<CheckIcon />} onClick={() => write("resolved")}>Resolve</Button></Tooltip>
              </>
            ) : (
              <Tooltip content="Unresolve (U)" asChild><Button variant="secondary" icon={<ArrowCounterClockwiseIcon />} onClick={() => write("unresolved")}>Unresolve</Button></Tooltip>
            )}
          </>
        }
      />
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-kumo-line">
        <div className="min-w-0 overflow-x-auto"><Tabs
          variant="underline"
          aria-label="Issue sections"
          value={section}
          onValueChange={(v) => setSection(v, true)}
          tabs={[
            { value: "overview", label: "Overview" },
            { value: "stack", label: "Stack trace" },
            { value: "breadcrumbs", label: label("Breadcrumbs", crumbCount) },
            { value: "events", label: label("Events", eventsQ.data ? events.length : undefined) },
            { value: "json", label: "JSON" },
          ]}
        /></div>
        {event && (
          <div className="tnum flex items-center gap-1 text-xs text-kumo-subtle">
            <Tooltip content="Older event ([)" asChild>
              <Button size="sm" variant="ghost" shape="square" aria-label="Older event" icon={<CaretLeftIcon />} onClick={() => void step(1)} />
            </Tooltip>
            <span>Event {index >= 0 ? `${index + 1}/${events.length}${eventsQ.hasNextPage ? "+" : ""}` : "–"}, {relative(event.timestamp)}, <span className="mono">{short(event.event_id)}</span></span>
            <Tooltip content="Newer event (])" asChild>
              <Button size="sm" variant="ghost" shape="square" aria-label="Newer event" icon={<CaretRightIcon />} onClick={() => void step(-1)} />
            </Tooltip>
          </div>
        )}
      </div>
      <div ref={outerScroller} className="flex min-h-0 flex-1 flex-col overflow-auto">
        {issueQ.isError && <StaleNote error={issueQ.error} />}
        {eventQ.isError && !event && <ErrorState error={eventQ.error} onRetry={() => void eventQ.refetch()} title="Could not load this event" />}
        {section === "overview" && <Overview issue={issue} event={event} w={w} />}
        {section === "stack" && <StackTrace event={event} loading={eventQ.isPending} inAppOnly={inAppOnly} setInAppOnly={setInAppOnly} raw={raw} setRaw={setRaw} />}
        {section === "breadcrumbs" && <Breadcrumbs event={event} />}
        {section === "events" && (
          <EventsList events={events} shownId={shownId} onShow={show} hasMore={!!eventsQ.hasNextPage} loadMore={loadOlder} loadingMore={eventsQ.isFetchingNextPage} />
        )}
        {section === "json" && <JsonView event={event} scrollRef={jsonScroller} />}
      </div>
    </Page>
  );
}
