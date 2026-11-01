import { Meter, Table } from "@cloudflare/kumo";
import { useQuery } from "@tanstack/react-query";
import { otel } from "@/api/otel";
import { useInterval } from "@/app/settings";
import { DataTable, ErrorState, Facts, Pad, Page, Section, TableSkeleton } from "@/components/ui-bits";
import { bytes, compact, ms, timestamp, uptime } from "@/lib/format";
import { cn } from "@/lib/utils";

const bad = (n: number | undefined, cls: string) => (n ? cls : "text-kumo-subtle");

export function StatusPage() {
  const refetchInterval = useInterval(5000);
  const q = useQuery({ queryKey: ["otel", "stats"], queryFn: ({ signal }) => otel.stats(signal), refetchInterval });
  if (q.isError && !q.data) return <Page><ErrorState error={q.error} onRetry={() => void q.refetch()} title="Could not load the server status" /></Page>;
  if (!q.data) return <Page><TableSkeleton /></Page>;
  const { ingest, storage, version, uptime_secs, capabilities } = q.data;
  const bufferPct = ingest.max_buffer_bytes ? (ingest.buffered_bytes / ingest.max_buffer_bytes) * 100 : 0;
  const tableBytes = storage.tables.reduce((a, t) => a + t.bytes, 0);
  const m = storage.last_maintenance;
  return (
    <Page scroll>
      <div className="grid items-start gap-4 2xl:grid-cols-[1fr_1.2fr]">
        <Section title="Ingest">
          <DataTable>
            <caption className="sr-only">Ingest per signal</caption>
            <Table.Header variant="compact" sticky><Table.Row><Table.Head>Signal</Table.Head><Table.Head className="text-right">Requests</Table.Head><Table.Head className="text-right">Accepted</Table.Head><Table.Head className="text-right">Throttled</Table.Head><Table.Head className="text-right">Invalid</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {ingest.signals.map((s) => (
                <Table.Row key={s.signal}>
                  <Table.Cell className="capitalize">{s.signal}</Table.Cell>
                  <Table.Cell className="text-right text-kumo-subtle">{compact(s.requests)}</Table.Cell>
                  <Table.Cell className="text-right font-medium">{compact(s.accepted)}</Table.Cell>
                  <Table.Cell className={cn("text-right", bad(s.throttled, "text-kumo-warning"))}>{compact(s.throttled ?? 0)}</Table.Cell>
                  <Table.Cell className={cn("text-right", bad(s.invalid, "text-kumo-danger"))}>{compact(s.invalid ?? 0)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </DataTable>
          <div className="flex flex-col gap-4 px-4 py-3">
            <Meter label={`Buffer: ${compact(ingest.buffered_rows)} rows`} value={Math.round(bufferPct)} customValue={`${bytes(ingest.buffered_bytes)} of ${bytes(ingest.max_buffer_bytes)}`} />
            <Facts rows={[
              ["Flushes", compact(ingest.flushes)],
              ["Flush errors", <span className={ingest.flush_errors ? "font-medium text-kumo-danger" : "text-kumo-success"}>{compact(ingest.flush_errors)}</span>],
              ["Last flush", ingest.last_flush ? `${compact(ingest.last_flush.rows)} rows in ${ms(ingest.last_flush.duration_ms)} at ${timestamp(ingest.last_flush.at)}` : "–"],
              ...(ingest.last_flush_error ? ([["Last error", <span className="text-kumo-danger">{ingest.last_flush_error}</span>]] as [string, React.ReactNode][]) : []),
            ]} />
          </div>
        </Section>
        <Section title="Storage" right={`telemetry ${version}, up ${uptime(uptime_secs)}`}>
          <Pad>
            <Facts rows={[
              ["Catalog", storage.catalog_type],
              ["Data", <span className="mono">{storage.data_path}</span>],
              ["Snapshot", String(storage.snapshot_id)],
            ]} />
          </Pad>
          <DataTable>
            <caption className="sr-only">Storage per table</caption>
            <Table.Header variant="compact" sticky><Table.Row><Table.Head>Table, {bytes(tableBytes)} in total</Table.Head><Table.Head className="text-right">Rows</Table.Head><Table.Head className="text-right">Inlined</Table.Head><Table.Head className="text-right">Files</Table.Head><Table.Head className="text-right">Size</Table.Head><Table.Head className="text-right">Avg file</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {storage.tables.map((t) => (
                <Table.Row key={t.name}>
                  <Table.Cell className="mono">{t.name}</Table.Cell>
                  <Table.Cell className="text-right">{compact(t.rows)}</Table.Cell>
                  <Table.Cell className="text-right text-kumo-subtle">{compact(t.inlined_rows)}</Table.Cell>
                  <Table.Cell className="text-right text-kumo-subtle">{compact(t.files)}</Table.Cell>
                  <Table.Cell className="text-right">{bytes(t.bytes)}</Table.Cell>
                  <Table.Cell className="text-right text-kumo-subtle">{t.files ? bytes(t.bytes / t.files) : "–"}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </DataTable>
          <Pad>
            {m ? (
              <p className={m.ok ? "" : "text-kumo-danger"}>Maintenance {m.task} {m.ok ? "ok" : "failed"} at {timestamp(m.at)} in {ms(m.duration_ms)}{m.detail ? `: ${m.detail}` : ""}</p>
            ) : (
              <p className="text-kumo-subtle">No maintenance run yet</p>
            )}
          </Pad>
        </Section>
        {capabilities && (
          <Section title="What the server allows" className="2xl:col-span-2">
            <Pad>
              <Facts rows={[
                ["Ad-hoc SQL", capabilities.sql ? "on" : "off"],
                ["Longest window", uptime(capabilities.max_window_secs)],
                ["Ingest needs a token", capabilities.ingest_auth ? "yes" : "no"],
                ["New data shows up after", `about ${capabilities.flush_interval_secs}s`],
              ]} />
            </Pad>
          </Section>
        )}
      </div>
    </Page>
  );
}
