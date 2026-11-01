import { get, getPage, type Page, type Params } from "./client";
import type {
  AttributeStats, LogRecord, LogSeries, MetricInfo, MetricQueryResult, OperationSummary, ServiceSummary, Span, SpanSeries, Stats, Trace,
  TraceSummary,
} from "./types";

const A = "/otel/api/v1";
type Sig = AbortSignal | undefined;

export const otel = {
  stats: (signal?: Sig) => get<Stats>(`${A}/stats`, undefined, signal),
  services: (since: string, signal?: Sig) => get<ServiceSummary[]>(`${A}/services`, { since }, signal),
  operations: (since: string, service: string[] | undefined, signal?: Sig) =>
    get<OperationSummary[]>(`${A}/operations`, { since, service }, signal),
  spanSeries: (since: string, service: string[] | undefined, signal?: Sig) =>
    get<SpanSeries>(`${A}/spans/series`, { since, service }, signal),
  attributes: (q: { signal: "spans" | "logs"; since: string; service?: string[]; key?: string; prefix?: string; limit?: number }, signal?: Sig) =>
    get<AttributeStats>(`${A}/attributes`, q, signal),
  logSeries: (q: Params, signal?: Sig) => get<LogSeries>(`${A}/logs/series`, q, signal),
  traces: (q: Params, signal?: Sig): Promise<Page<TraceSummary[]>> =>
    getPage<TraceSummary[]>(`${A}/traces`, { limit: 200, ...q }, signal),
  trace: (id: string, q: Params, signal?: Sig) => get<Trace>(`${A}/traces/${encodeURIComponent(id)}`, q, signal),
  logs: (q: Params, signal?: Sig) => get<LogRecord[]>(`${A}/logs`, q, signal),
  metrics: (since: string, service: string[] | undefined, signal?: Sig) => get<MetricInfo[]>(`${A}/metrics`, { since, service }, signal),
  metric: (q: Params, signal?: Sig) => get<MetricQueryResult>(`${A}/metrics/query`, q, signal),
};

export type { Span };
