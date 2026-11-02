import { useMemo } from "react";
import { Button, Select, Table, Tabs, Tooltip, useKumoToastManager } from "@cloudflare/kumo";
import { ArrowCounterClockwiseIcon, BugIcon, CheckIcon, EyeSlashIcon } from "@phosphor-icons/react";
import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Link, Navigate, useLocation, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { sentry } from "@/api/sentry";
import type { IssueSummary, ProjectStats } from "@/api/types";
import { leaveMenu } from "@/app/keys";
import { useInterval } from "@/app/settings";
import { useParam, useScope, useWindow } from "@/app/window";
import { NoErrors, WidenButton } from "@/components/scope-states";
import { issueSuggestions } from "@/components/suggestions";
import { StackedBars } from "@/components/charts";
import { DeepListNote, firstPageOnly, isDeep, useListNav } from "@/components/list";
import { Ago, DataTable, EmptyState, ErrorState, Level, Page, SearchField, Section, StaleNote, Stat, Status, TableSkeleton } from "@/components/ui-bits";
import { Sparkline } from "@/components/charts";
import { clean } from "@/lib/clean";
import { intervalOf, SORTS, STATUSES, type StatusFilter } from "@/lib/domain";
import { age, compact, timestamp } from "@/lib/format";
import { parseIssueFilter } from "@/lib/query";
import { useProjects, useSetStatus } from "./data";

/** `/errors`: the selected app's issues, the fix if it has no project, else a choice:
 *  issues are per project, there is no list across them (ADR 0028). */
export function ErrorsLanding() {
  const { app, apps, project, loading, loaded, error, setApp } = useScope();
  const { search } = useLocation();
  const withProject = apps.filter((a) => a.project !== null && a.project !== undefined);
  const { selectedId, select } = useListNav({ listKey: "pick-app", items: withProject, idOf: (a) => a.slug, onOpen: (a) => setApp(a.slug) });
  if (error && !apps.length) return <Page><ErrorState error={error} title="Could not load the apps" /></Page>;
  if (app && loading) return <TableSkeleton />;
  if (project) return <Navigate to={{ pathname: `/errors/${encodeURIComponent(project)}`, search }} replace />;
  if (app) return <Page><Section><NoErrors /></Section></Page>;
  if (!apps.length && !error && !loaded) return <TableSkeleton />;
  if (!withProject.length) {
    return (
      <Page>
        <Section>
          <EmptyState
            icon={<BugIcon size={32} />}
            title="No projects yet"
            description="Create one, send your first error, and it shows up here."
            commandLine="mallard setup my-app --platform node"
          />
        </Section>
      </Page>
    );
  }
  return (
    <Page>
      <Section className="min-h-0 shrink!" title="Pick an app to see its issues" right={`${withProject.length} with error tracking`}>
        <div className="min-h-0 flex-1 overflow-auto">
          <DataTable fixed>
            <caption className="sr-only">Apps with error tracking. Enter shows the issues of one.</caption>
            <Table.Header variant="compact" sticky><Table.Row><Table.Head>App</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {withProject.map((a) => (
                <Table.Row key={a.slug} data-row-id={a.slug} aria-selected={selectedId === a.slug} variant={selectedId === a.slug ? "selected" : "default"} onClick={() => select(a.slug)} onDoubleClick={() => setApp(a.slug)}>
                  <Table.Cell className="max-w-0">
                    <Link to={`/errors/${encodeURIComponent(a.slug)}`} onClick={(e) => e.stopPropagation()} className="block truncate font-medium hover:underline" title={a.name}>{a.name}</Link>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </DataTable>
        </div>
      </Section>
    </Page>
  );
}

export function useProjectStats(project: string | null, w: string) {
  const interval = intervalOf(w);
  const refetchInterval = useInterval(5000);
  return useQuery({
    queryKey: ["sentry", "stats", project, w, interval],
    enabled: !!project,
    queryFn: ({ signal }) => sentry.stats(project!, w, interval, signal),
    refetchInterval,
    placeholderData: keepPreviousData,
  });
}

/** Events over the window, split by level. */
export function EventBars({ stats, w }: { stats: ProjectStats | undefined; w: string }) {
  const buckets = useMemo(
    () => (stats?.buckets ?? []).map((b) => ({ time: Date.parse(b.start), values: { errors: b.errors, warnings: b.warnings, other: b.other } })),
    [stats],
  );
  const peak = Math.max(0, ...(stats?.buckets ?? []).map((b) => b.events));
  return (
    <>
      <div className="tnum mb-1 flex justify-between gap-2 text-kumo-subtle">
        <span>Events per {intervalOf(w)}</span>
        {stats && <span className="text-xs">peak {compact(peak)}</span>}
      </div>
      <StackedBars
        height={32}
        buckets={buckets}
        series={[
          { key: "errors", label: "Errors", color: "var(--chart-attention)" },
          { key: "warnings", label: "Warnings", color: "var(--chart-warning)" },
          { key: "other", label: "Other", color: "var(--chart-neutral)" },
        ]}
        axis={[`-${w}`, "now"]}
        ariaLabel={stats ? `Events in the last ${w}: ${compact(stats.total_events)} in total, peak ${compact(peak)}` : "Loading events"}
        stepLabel={(t) => `${timestamp(new Date(t))} UTC`}
      />
    </>
  );
}

function StatsStrip({ project, w }: { project: string; w: string }) {
  const stats = useProjectStats(project, w);
  const s: ProjectStats | undefined = stats.data;
  const hours = s ? Math.max((Date.parse(s.window_end) - Date.parse(s.window_start)) / 3_600_000, 1 / 60) : 1;
  return (
    <Section>
      <div className="grid @3xl:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)]">
        <div className="grid grid-cols-4 divide-x divide-kumo-hairline">
          <Stat label="Events" value={s ? compact(s.total_events) : "–"} sub={s ? `${(s.total_events / hours).toFixed(1)}/h` : " "} />
          <Stat label="Issues" value={s ? compact(s.unique_issues) : "–"} sub="with events" />
          <Stat label="New" value={s ? compact(s.new_issues) : "–"} sub="first seen" />
          <Stat label="Users" value={s ? compact(s.affected_users) : "–"} sub="affected" />
        </div>
        <div className="hidden min-w-0 border-l border-kumo-hairline px-4 py-3 @3xl:block">
          {stats.isError && !s ? <p className="py-3 text-xs text-kumo-danger">{stats.error.message}</p> : <EventBars stats={s} w={w} />}
        </div>
      </div>
    </Section>
  );
}

function IssueRow({ issue, project, selected, onSelect, onOpen, onStatus, windowLabel }: {
  issue: IssueSummary;
  project: string;
  selected: boolean;
  onSelect: () => void;
  onOpen: () => void;
  onStatus: (status: "resolved" | "ignored" | "unresolved") => void;
  windowLabel: string;
}) {
  return (
    <Table.Row data-row-id={issue.id} aria-selected={selected} variant={selected ? "selected" : "default"} onClick={onSelect} onDoubleClick={onOpen}>
      <Table.Cell className="hidden @xl:table-cell"><Level level={issue.level} /></Table.Cell>
      <Table.Cell className="max-w-0">
        <Link to={`/errors/${encodeURIComponent(project)}/issues/${issue.id}`} onClick={(e) => e.stopPropagation()} className="block truncate font-medium text-kumo-default hover:underline" title={issue.title}>
          {issue.title}
        </Link>
        <div className="mono truncate text-kumo-subtle" title={issue.culprit ?? undefined}>{issue.culprit || "–"}</div>
      </Table.Cell>
      <Table.Cell className="text-right">{compact(issue.count)}</Table.Cell>
      <Table.Cell className="hidden text-right @3xl:table-cell">{compact(issue.user_count)}</Table.Cell>
      <Table.Cell className="hidden @2xl:table-cell"><Sparkline values={issue.trend} label={`Events in the last ${windowLabel}`} /></Table.Cell>
      <Table.Cell><Ago t={issue.last_seen} /></Table.Cell>
      <Table.Cell className="hidden text-kumo-subtle @6xl:table-cell" title={`First seen ${timestamp(issue.first_seen)} UTC`}>{age(issue.first_seen)}</Table.Cell>
      <Table.Cell className="hidden @5xl:table-cell"><Status status={issue.status} /></Table.Cell>
      <Table.Cell className="hidden whitespace-nowrap @lg:table-cell" onClick={(e) => e.stopPropagation()}>
        {issue.status === "unresolved" ? (
          <span className="flex gap-0.5">
            <Tooltip content="Resolve (R)" asChild>
              <Button size="sm" variant="ghost" shape="square" aria-label={`Resolve ${issue.title}`} icon={<CheckIcon />} onClick={() => onStatus("resolved")} />
            </Tooltip>
            <Tooltip content="Ignore (I)" asChild>
              <Button size="sm" variant="ghost" shape="square" aria-label={`Ignore ${issue.title}`} icon={<EyeSlashIcon />} onClick={() => onStatus("ignored")} />
            </Tooltip>
          </span>
        ) : (
          <Tooltip content="Unresolve (U)" asChild>
            <Button size="sm" variant="ghost" shape="square" aria-label={`Unresolve ${issue.title}`} icon={<ArrowCounterClockwiseIcon />} onClick={() => onStatus("unresolved")} />
          </Tooltip>
        )}
      </Table.Cell>
    </Table.Row>
  );
}

export function IssuesPage() {
  const { project = "" } = useParams();
  const navigate = useNavigate();
  const { w } = useWindow();
  const [status, setStatus] = useParam("status", "unresolved");
  const [query, setQuery] = useParam("q");
  const [sort, setSort] = useParam("sort", "last_seen");
  const [params] = useSearchParams();
  const toasts = useKumoToastManager();
  const projects = useProjects();
  const refetchInterval = useInterval(5000);
  const setIssueStatus = useSetStatus(project);
  const parsedFilter = parseIssueFilter(query);
  const filter = "error" in parsedFilter ? { tags: [] as string[], query: undefined } : parsedFilter;

  const current = projects.data?.find((p) => p.slug === project || String(p.id) === project);

  const issues = useInfiniteQuery({
    queryKey: ["sentry", "issues", project, status, query, w, sort],
    queryFn: ({ signal, pageParam }) => sentry.issues(project, { status, query: filter.query, tag: filter.tags, since: w, sort, cursor: pageParam }, signal),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    refetchInterval: firstPageOnly(refetchInterval),
    placeholderData: keepPreviousData,
  });
  const items = useMemo(() => issues.data?.pages.flatMap((p) => p.issues) ?? [], [issues.data]);
  const open = (i: IssueSummary) => navigate(`/errors/${encodeURIComponent(project)}/issues/${i.id}?w=${w}`);

  const statusIdx = STATUSES.indexOf(status as StatusFilter);
  const stepStatus = (dir: 1 | -1) => setStatus(STATUSES[(statusIdx + dir + STATUSES.length) % STATUSES.length]!);
  const stepSort = () => {
    const i = SORTS.findIndex((s) => s.value === sort);
    setSort(SORTS[(i + 1) % SORTS.length]!.value);
  };
  const write = (next: "resolved" | "ignored" | "unresolved", issue?: IssueSummary) => {
    if (!issue) return;
    if (issue.status === next) toasts.add({ title: `Already ${next}` });
    else setIssueStatus.mutate({ id: issue.id, title: issue.title, status: next });
  };

  const { selectedId, select, selected } = useListNav({
    listKey: `issues:${project}`,
    items,
    idOf: (i) => i.id,
    onOpen: open,
    keys: {
      s: stepSort,
      l: () => stepStatus(1),
      h: () => stepStatus(-1),
      ArrowRight: () => stepStatus(1),
      ArrowLeft: () => stepStatus(-1),
      Escape: () => query && setQuery(""),
      R: () => write("resolved", selected),
      I: () => write("ignored", selected),
      U: () => write("unresolved", selected),
      ...Object.fromEntries(STATUSES.map((s, i) => [String(i + 1), () => setStatus(s)])),
    },
  });

  const suggest = useMemo(() => issueSuggestions(project, w), [project, w]);
  const hasMore = issues.hasNextPage;
  const countLabel = `${compact(items.length)}${hasMore ? "+" : ""}`;

  if (projects.data && !current) {
    return (
      <Page>
        <EmptyState
          title={`Project "${clean(project)}" not found`}
          description="It may have been renamed. Pick another app."
          action={<Button onClick={() => navigate("/errors?app=")}>Show the apps</Button>}
        />
      </Page>
    );
  }

  return (
    <Page>
      <StatsStrip project={project} w={w} />
      <div className="flex shrink-0 flex-wrap items-center gap-2">
        <Tabs

          variant="segmented"
          aria-label="Issue status"
          value={status}
          onValueChange={(v) => setStatus(v)}
          tabs={STATUSES.map((s) => ({ value: s, label: s === status ? `${s[0]!.toUpperCase()}${s.slice(1)} ${countLabel}` : `${s[0]!.toUpperCase()}${s.slice(1)}` }))}
        />
        <SearchField
          value={query}
          onApply={setQuery}
          label="Search issues"
          placeholder="Search issues, or tag=value"
          suggest={suggest}
          validate={(v) => {
            const p = parseIssueFilter(v);
            return "error" in p ? p.error : null;
          }}
        />
        <Select
          aria-label="Sort"
          className="w-36"
          value={sort}
          onValueChange={(v) => v && setSort(v as string)}
          onOpenChange={leaveMenu}
          items={Object.fromEntries(SORTS.map((s) => [s.value, s.label]))}
        />
      </div>
      <Section className="min-h-0 flex-1 shrink!">
        {issues.isError && issues.data && <StaleNote error={issues.error} />}
        {issues.isError && !issues.data ? (
          <ErrorState error={issues.error} onRetry={() => void issues.refetch()} title="Could not load issues" />
        ) : issues.isPending ? (
          <TableSkeleton />
        ) : items.length === 0 ? (
          <EmptyState
            icon={query ? undefined : <CheckIcon size={32} className="text-kumo-success" />}
            title={`No ${status === "all" ? "" : `${clean(status)} `}issues${query ? ` matching "${clean(query)}"` : ""} in the last ${w}`}
            description={query ? "Clear the search, or widen the time window." : status === "unresolved" ? "Nothing to fix here. Widen the time window to look further back." : "Widen the time window or look at another status."}
            action={
              <div className="flex gap-2">
                {query && <Button onClick={() => setQuery("")}>Clear search</Button>}
                <WidenButton />
              </div>
            }
          />
        ) : (
          <div className="min-h-0 flex-1 overflow-auto" aria-busy={issues.isFetching}>
            <DataTable fixed>
              <caption className="sr-only">Issues, {params.get("status") ?? "unresolved"}. Use j and k to move, Enter to open.</caption>
              <Table.Header variant="compact" sticky>
                <Table.Row>
                  <Table.Head className="hidden w-24 @xl:table-cell">Level</Table.Head>
                  <Table.Head>Issue</Table.Head>
                  <Table.Head className="w-20 text-right">Events</Table.Head>
                  <Table.Head className="hidden w-20 text-right @3xl:table-cell">Users</Table.Head>
                  <Table.Head className="hidden w-32 @2xl:table-cell">Trend</Table.Head>
                  <Table.Head className="w-24">Last seen</Table.Head>
                  <Table.Head className="hidden w-16 @6xl:table-cell">Age</Table.Head>
                  <Table.Head className="hidden w-32 @5xl:table-cell">Status</Table.Head>
                  <Table.Head className="hidden w-20 @lg:table-cell"><span className="sr-only">Actions</span></Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {items.map((issue) => (
                  <IssueRow
                    key={issue.id}
                    issue={issue}
                    project={project}
                    selected={issue.id === selectedId}
                    onSelect={() => select(issue.id)}
                    onOpen={() => open(issue)}
                    onStatus={(s) => write(s, issue)}
                    windowLabel={w}
                  />
                ))}
              </Table.Body>
            </DataTable>
            {isDeep(issues.data) && <DeepListNote />}
            {hasMore ? (
              <div className="flex justify-center px-4 py-3">
                <Button variant="secondary" loading={issues.isFetchingNextPage} onClick={() => void issues.fetchNextPage()}>Load more</Button>
              </div>
            ) : (
              <p className="px-4 py-3 text-center text-xs text-kumo-subtle">End of the list</p>
            )}
          </div>
        )}
      </Section>
    </Page>
  );
}
