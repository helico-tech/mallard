import { get, getPage, send } from "./client";
import type {
  EventDetail, EventList, EventsByTrace, IssueDetail, IssueList, IssueStatus, Project, ProjectStats, ServerInfo,
} from "./types";

const p = (project: string) => `/sentry/v1/projects/${encodeURIComponent(project)}`;
type Sig = AbortSignal | undefined;

export const sentry = {
  info: (signal?: Sig) => get<ServerInfo>("/sentry/v1/info", undefined, signal),
  projects: (signal?: Sig) => get<Project[]>("/sentry/v1/projects", undefined, signal),
  issues: (
    project: string,
    q: { status: string; query?: string; since: string; sort: string; cursor?: string },
    signal?: Sig,
  ) => get<IssueList>(`${p(project)}/issues`, { ...q, limit: 200 }, signal),
  stats: (project: string, since: string, interval: string, signal?: Sig) =>
    get<ProjectStats>(`${p(project)}/stats`, { since, interval }, signal),
  issue: (project: string, id: string, since: string, signal?: Sig) =>
    get<IssueDetail>(`${p(project)}/issues/${encodeURIComponent(id)}`, { since }, signal),
  events: (project: string, issue: string, cursor?: string, signal?: Sig) =>
    get<EventList>(`${p(project)}/events`, { issue, limit: 50, before: cursor }, signal),
  event: (project: string, eventId: string, issue: string, signal?: Sig) =>
    get<EventDetail>(`${p(project)}/events/${encodeURIComponent(eventId)}`, { issue }, signal),
  setStatus: (project: string, id: string, status: IssueStatus) =>
    send<IssueDetail>("PATCH", `${p(project)}/issues/${encodeURIComponent(id)}`, { status }),
  eventsOfTrace: (traceId: string, signal?: Sig) =>
    get<EventList>("/sentry/v1/events", { trace_id: traceId }, signal),
  errorsByTrace: (ids: string[], from: string, to: string, signal?: Sig) =>
    get<EventsByTrace>("/sentry/v1/events/by-trace", { trace_id: ids.join(","), from, to }, signal),
};

export { getPage };
