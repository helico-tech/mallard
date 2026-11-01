// Small shared pieces. Text always comes with colour, never colour alone.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Badge, Banner, Button, Empty, InlineCopyText, InputGroup, LayerCard, SkeletonLine, Table, Text, Tooltip } from "@cloudflare/kumo";
import { ArrowsClockwiseIcon, MagnifyingGlassIcon, WarningCircleIcon } from "@phosphor-icons/react";
import { ApiError } from "@/api/client";
import { Kbd } from "@/components/ui/kbd";
import { backTarget, sideOf } from "@/app/history";
import { useKeys } from "@/app/keys";
import { useLocation, useNavigate } from "react-router-dom";
import { levelLabel, levelTone, serviceColor, severityTone, type Tone } from "@/lib/domain";
import { relative, timestamp } from "@/lib/format";
import { cn } from "@/lib/utils";

const TONE_TEXT: Record<Tone, string> = {
  fatal: "text-kumo-danger",
  error: "text-kumo-danger",
  warn: "text-kumo-warning",
  info: "text-kumo-info",
  debug: "text-kumo-subtle",
  muted: "text-kumo-subtle",
};

const TONE_BG: Record<Tone, string> = {
  fatal: "bg-kumo-danger",
  error: "bg-kumo-danger",
  warn: "bg-kumo-warning",
  info: "bg-kumo-info",
  debug: "bg-kumo-fill",
  muted: "bg-kumo-fill",
};

export const toneText = (t: Tone) => TONE_TEXT[t];
export const toneBg = (t: Tone) => TONE_BG[t];

const LEVEL_BADGE: Record<Tone, "error" | "warning" | "neutral"> = {
  fatal: "error", error: "error", warn: "warning", info: "neutral", debug: "neutral", muted: "neutral",
};

/** The level as a word with a dot: never colour alone. */
export function Level({ level }: { level: string }) {
  return <Badge appearance="dot" variant={LEVEL_BADGE[levelTone(level)]}>{levelLabel(level)}</Badge>;
}

const STATUS_BADGE: Record<string, "warning" | "success" | "neutral"> = {
  unresolved: "warning", resolved: "success", ignored: "neutral",
};

export function Status({ status }: { status: string }) {
  return <Badge appearance="dot" variant={STATUS_BADGE[status] ?? "neutral"}>{status[0]?.toUpperCase()}{status.slice(1)}</Badge>;
}

export function Service({ name, className }: { name: string; className?: string }) {
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)}>
      <span aria-hidden className="size-2 shrink-0 rounded-full" style={{ background: serviceColor(name) }} />
      <span className="truncate" title={name}>{name}</span>
    </span>
  );
}

/** Refreshes itself so "3m ago" stays true between loads. */
export function Ago({ t, className }: { t: string; className?: string }) {
  useTick(15_000);
  return (
    <span className={cn("tnum whitespace-nowrap", className)} title={`${timestamp(t)} UTC`}>
      {relative(t)}
    </span>
  );
}

/** One word for a severity everywhere: coloured, and readable without the colour. */
export function Severity({ name }: { name: string }) {
  const word = name === "-" ? "–" : name[0] + name.slice(1).toLowerCase();
  return <span className={cn("font-medium", TONE_TEXT[severityTone(name)])}>{word}</span>;
}

/** Esc: up this side's back stack, as in the TUI; `fallback` on a deep link. */
export function useBack(fallback: string): () => void {
  const navigate = useNavigate();
  const { pathname } = useLocation();
  return () => navigate(backTarget(sideOf(pathname), fallback));
}

/** Below this width a split view stacks. */
export function useNarrow(): boolean {
  const [narrow, setNarrow] = useState(() => window.matchMedia("(max-width: 1279px)").matches);
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 1279px)");
    const onChange = (e: MediaQueryListEvent) => setNarrow(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return narrow;
}

export function useTick(ms: number): number {
  const [n, setN] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setN((x) => x + 1), ms);
    return () => clearInterval(id);
  }, [ms]);
  return n;
}

export function CopyId({ value, children }: { value: string; children?: ReactNode }) {
  return (
    <InlineCopyText value={value} variant="mono-secondary" className="align-baseline text-xs">
      {children ?? value}
    </InlineCopyText>
  );
}

export function KeyHint({ children }: { children: ReactNode }) {
  return <Kbd className="bg-kumo-recessed font-mono text-xs text-kumo-subtle">{children}</Kbd>;
}

export function Hint({ label, keys, children }: { label: string; keys?: string; children: React.ReactElement }) {
  return (
    <Tooltip
      content={
        <span className="inline-flex items-center gap-2">
          {label}
          {keys && <KeyHint>{keys}</KeyHint>}
        </span>
      }
      asChild
    >
      {children}
    </Tooltip>
  );
}

function errorParts(error: unknown): { message: string; hint: string | null } {
  if (error instanceof ApiError) return { message: error.message, hint: error.hint };
  return { message: error instanceof Error ? error.message : String(error), hint: null };
}

/** What failed, what to try, and Retry. */
export function ErrorState({ error, onRetry, title = "Could not load this" }: { error: unknown; onRetry?: () => void; title?: string }) {
  const { message, hint } = errorParts(error);
  return (
    <div className="px-4 py-3">
      <Banner
        variant="error"
        icon={<WarningCircleIcon weight="fill" />}
        title={title}
        description={hint ? `${message}. ${hint}` : message}
        action={
          onRetry && (
            <Button variant="secondary" onClick={onRetry} icon={<ArrowsClockwiseIcon />}>
              Retry
            </Button>
          )
        }
      />
    </div>
  );
}

/** Data stays on screen; this says it is old. */
export function StaleNote({ error }: { error: unknown }) {
  const { message } = errorParts(error);
  return (
    <p role="status" className="flex items-start gap-1.5 px-4 py-1 text-kumo-warning">
      <span className="flex h-lh items-center"><WarningCircleIcon aria-hidden /></span>
      Refresh failed, showing the last data: {message.slice(0, 120)}
    </p>
  );
}

export function EmptyState(props: { title: string; description?: string; commandLine?: string; action?: ReactNode; icon?: ReactNode }) {
  return (
    <div className="grid place-items-center px-8 py-6">
      <Empty size="sm" className="border-0 bg-transparent p-0 ring-0" title={props.title} description={props.description} commandLine={props.commandLine} icon={props.icon} contents={props.action} />
    </div>
  );
}

export function TableSkeleton({ rows = 8 }: { rows?: number }) {
  return (
    <div className="flex flex-col gap-3 px-4 py-3" aria-label="Loading" role="status">
      {Array.from({ length: rows }, (_, i) => <SkeletonLine key={i} />)}
    </div>
  );
}

/** Every screen: the same padding and the same gap. */
export function Page({ children, scroll }: { children: ReactNode; scroll?: boolean }) {
  return <div className={cn("flex h-full min-h-0 flex-col gap-4 px-5 py-4", scroll ? "overflow-auto" : "overflow-hidden")}>{children}</div>;
}

/** The one panel: a LayerCard, with a title row when it has a title. Children
 *  bring their own padding only through `Pad`, so tables can run edge to edge. */
export function Section({ title, right, children, className }: { title?: ReactNode; right?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <LayerCard className={cn("min-w-0 shrink-0", className)}>
      {(title || right) && (
        <LayerCard.Secondary className="flex items-center justify-between gap-3">
          <Text as="h2" bold>{title}</Text>
          {right && <div className="tnum flex shrink-0 items-center gap-2 text-xs font-normal text-kumo-subtle">{right}</div>}
        </LayerCard.Secondary>
      )}
      <LayerCard.Primary className="@container min-h-0 flex-1 gap-0 overflow-auto p-0">{children}</LayerCard.Primary>
    </LayerCard>
  );
}

export function Pad({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("px-4 py-3", className)}>{children}</div>;
}

/** The one data table: Kumo's Table, tighter rows, a clickable row when it has an id. */
/** The header of a list that is not a table (waterfall, logs): the same look. */
export const LIST_HEADER = "tnum grid shrink-0 border-b border-kumo-fill bg-kumo-elevated py-1.5 font-semibold";

export function DataTable({ children, fixed, className }: { children: ReactNode; fixed?: boolean; className?: string }) {
  return (
    <Table layout={fixed ? "fixed" : "auto"} className={cn("data-table tnum w-full", className)}>
      {children}
    </Table>
  );
}

/** The one search box: `/` focuses it, Enter applies, Esc leaves it. A bad filter
 *  says why and the old one stays. `live` applies on every key. */
export function SearchField({ value, onApply, label, placeholder, validate, live, mono, className }: {
  value: string;
  onApply: (v: string) => void;
  label: string;
  placeholder: string;
  validate?: (v: string) => string | null;
  live?: boolean;
  mono?: boolean;
  className?: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const [draft, setDraft] = useState(value);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setDraft(value);
    setError(null);
  }, [value]);
  useKeys({ "/": () => ref.current?.focus(), f: () => ref.current?.focus() });
  const apply = (v: string) => {
    const problem = validate?.(v) ?? null;
    setError(problem);
    if (problem) return;
    onApply(v.trim());
    ref.current?.blur();
  };
  return (
    <form role="search" className={cn("min-w-52 flex-1", className)} onSubmit={(e) => { e.preventDefault(); apply(draft); }}>
      <InputGroup>
        <InputGroup.Addon><MagnifyingGlassIcon aria-hidden /></InputGroup.Addon>
        <InputGroup.Input
          ref={ref}
          aria-label={label}
          aria-invalid={!!error}
          className={mono ? "mono" : undefined}
          placeholder={placeholder}
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            if (live) onApply(e.target.value);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setDraft(value);
              setError(null);
              e.currentTarget.blur();
            }
          }}
        />
      </InputGroup>
      {error && <p role="alert" className="mt-1 text-xs text-kumo-danger">{error}</p>}
    </form>
  );
}

/** The top of a detail screen: heading, one line of facts, actions on the right. */
export function DetailHeader({ title, meta, actions }: { title: ReactNode; meta?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="flex shrink-0 flex-wrap items-start justify-between gap-x-4 gap-y-2">
      <div className="flex min-w-0 flex-1 basis-96 flex-col gap-1">
        <h2 className="text-lg font-semibold [overflow-wrap:anywhere]">{title}</h2>
        {meta && <div className="tnum flex flex-wrap items-center gap-x-3 gap-y-0.5 text-kumo-subtle">{meta}</div>}
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
    </div>
  );
}

export function Stat({ label, value, sub, valueClass }: { label: string; value: ReactNode; sub?: ReactNode; valueClass?: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1 px-4 py-3">
      <Text variant="secondary">{label}</Text>
      <div className={cn("tnum truncate text-2xl font-semibold text-kumo-default", valueClass)}>{value}</div>
      {sub && <div className="tnum truncate text-kumo-subtle" title={typeof sub === "string" ? sub : undefined}>{sub}</div>}
    </div>
  );
}

/** Key/value rows, with a real value column so long values wrap. */
export function Facts({ rows, className }: { rows: [string, ReactNode][]; className?: string }) {
  return (
    <dl className={cn("grid grid-cols-[minmax(0,min(168px,35%))_minmax(0,1fr)] items-baseline gap-x-4 gap-y-1", className)}>
      {rows.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="truncate text-kumo-subtle" title={k}>{k}</dt>
          <dd className="min-w-0 [overflow-wrap:anywhere]">{v ?? "–"}</dd>
        </div>
      ))}
    </dl>
  );
}

export function Attributes({ attrs, title }: { attrs: Record<string, string>; title: string }) {
  const entries = Object.entries(attrs);
  if (!entries.length) return null;
  return (
    <div>
      <h3 className="mb-1 font-medium">{title} ({entries.length})</h3>
      <Facts className="mono" rows={entries.sort(([a], [b]) => a.localeCompare(b))} />
    </div>
  );
}
