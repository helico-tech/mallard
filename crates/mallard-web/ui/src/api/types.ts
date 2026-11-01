// API shapes, generated from docs/*/openapi.json (`npm run api-types`).
import type { components as A } from "./app.gen";
import type { components as O } from "./otel.gen";
import type { components as S } from "./sentry.gen";

type SS = S["schemas"];
type OS = O["schemas"];
type AS = A["schemas"];

export type App = AS["App"];
export type AppList = AS["AppList"];
export type FindMatch = AS["FindMatch"];
export type FindResult = AS["FindResult"];

// The wire sends these enums in lower case; the OpenAPI document does not know that.
type Wire<T, K extends keyof T> = Omit<T, K> & { [P in K]: string };

export type IssueSummary = SS["IssueSummary"];
export type IssueDetail = SS["IssueDetail"];
export type IssueList = SS["IssueList"];
export type IssueStatus = SS["IssueStatus"];
export type EventSummary = SS["EventSummary"];
export type EventList = SS["EventList"];
export type EventDetail = Omit<SS["EventDetail"], "payload"> & { payload: unknown };
export type EventsByTrace = SS["EventsByTrace"];
export type TraceEvent = SS["TraceEvent"];
export type Project = SS["Project"];
export type ProjectStats = SS["ProjectStats"];
export type ServerInfo = SS["ServerInfo"];
export type TagStats = SS["TagStats"];

export type Span = Wire<OS["Span"], "kind" | "status_code">;
export type Trace = { trace_id: string; spans: Span[] };
export type TraceSummary = OS["TraceSummary"];
export type LogRecord = OS["LogRecord"];
export type LogSeries = OS["LogSeries"];
export type AttributeStats = OS["AttributeStats"];
export type SpanSeries = OS["SpanSeries"];
export type ServiceSummary = OS["ServiceSummary"];
export type OperationSummary = Wire<OS["OperationSummary"], "kind">;
export type MetricInfo = Wire<OS["MetricInfo"], "metric_type" | "temporality">;
export type MetricQueryResult = Wire<OS["MetricQueryResult"], "metric_type">;
export type Stats = Omit<OS["Stats"], "ingest" | "storage"> & {
  ingest: Omit<OS["IngestStats"], "signals"> & {
    signals: (Omit<OS["SignalIngestStats"], "signal"> & { signal: string })[];
  };
  storage: Omit<OS["StorageStats"], "catalog_type"> & { catalog_type: string };
};
