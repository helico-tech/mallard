import { useEffect, useMemo } from "react";
import { Button, Table } from "@cloudflare/kumo";
import { GraphIcon } from "@phosphor-icons/react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { sentry } from "@/api/sentry";
import type { EventSummary } from "@/api/types";
import { useKeys } from "@/app/keys";
import { useListNav } from "@/components/list";
import { useBack, Ago, CopyId, DataTable, DetailHeader, EmptyState, ErrorState, Level, Page, Section, TableSkeleton } from "@/components/ui-bits";
import { short } from "@/lib/format";
import { useProjects } from "./data";

/** The error events of one trace, across projects: where `x` lands from telemetry. */
export function TraceErrorsPage() {
  const { traceId = "" } = useParams();
  const [params] = useSearchParams();
  const wanted = params.get("event");
  const navigate = useNavigate();
  const projects = useProjects();
  const q = useQuery({ queryKey: ["sentry", "by-trace", traceId], queryFn: ({ signal }) => sentry.eventsOfTrace(traceId, signal) });
  const events = useMemo(() => q.data?.events ?? [], [q.data]);

  const slugOf = (id: number) => projects.data?.find((p) => p.id === id)?.slug ?? String(id);
  const open = (e: EventSummary) =>
    navigate(`/errors/${encodeURIComponent(slugOf(e.project_id))}/issues/${e.issue_id}?event=${e.event_id}&section=stack`);

  const back = useBack("/errors");
  useKeys({ Escape: back, q: back, Backspace: back, x: () => navigate(`/telemetry/traces/${traceId}`) });
  const { selectedId, select } = useListNav({ listKey: `by-trace:${traceId}`, items: events, idOf: (e) => e.event_id, onOpen: open });

  // A jump for one event opens it straight away.
  useEffect(() => {
    if (!wanted || !projects.data) return;
    const e = events.find((x) => x.event_id === wanted);
    if (e) navigate(`/errors/${encodeURIComponent(slugOf(e.project_id))}/issues/${e.issue_id}?event=${e.event_id}&section=stack`, { replace: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wanted, events, projects.data]);

  return (
    <Page>
      <DetailHeader
        title="Errors of a trace"
        meta={<>
          <span>Trace <CopyId value={traceId} /></span>
          {q.data && <span>{events.length} error events</span>}
        </>}
        actions={<Button variant="secondary" icon={<GraphIcon />} onClick={() => navigate(`/telemetry/traces/${traceId}`)}>Open the trace</Button>}
      />
      {q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} title="Could not load the errors of this trace" />
      ) : q.isPending ? (
        <TableSkeleton />
      ) : events.length === 0 ? (
        <EmptyState title="No error events for this trace" description="Sentry saw no errors in this trace, in the last 30 days." />
      ) : (
        <Section title="Error events" className="min-h-0 flex-1 shrink!">
          <DataTable fixed>
            <caption className="sr-only">Error events of the trace</caption>
            <Table.Header variant="compact" sticky><Table.Row><Table.Head className="w-24">Level</Table.Head><Table.Head>Event</Table.Head><Table.Head className="hidden w-28 @xl:table-cell">Project</Table.Head><Table.Head className="hidden w-28 @3xl:table-cell">Id</Table.Head><Table.Head className="hidden w-44 @4xl:table-cell">Span</Table.Head><Table.Head className="w-24">When</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {events.map((e) => (
                <Table.Row key={e.event_id} data-row-id={e.event_id} onClick={() => select(e.event_id)} onDoubleClick={() => open(e)} variant={selectedId === e.event_id ? "selected" : "default"}>
                  <Table.Cell><Level level={e.level} /></Table.Cell>
                  <Table.Cell className="max-w-0 truncate"><button type="button" onClick={() => open(e)} className="max-w-full truncate font-medium text-kumo-default hover:underline" title={e.title}>{e.title}</button></Table.Cell>
                  <Table.Cell className="hidden truncate text-kumo-subtle @xl:table-cell">{slugOf(e.project_id)}</Table.Cell>
                  <Table.Cell className="mono hidden text-kumo-subtle @3xl:table-cell">{short(e.event_id, 10)}</Table.Cell>
                  <Table.Cell className="mono hidden text-kumo-subtle @4xl:table-cell">{e.span_id ?? "–"}</Table.Cell>
                  <Table.Cell><Ago t={e.timestamp} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </DataTable>
        </Section>
      )}
    </Page>
  );
}
