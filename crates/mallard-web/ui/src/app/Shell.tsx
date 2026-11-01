// The frame every screen sits in: side nav (the F1/F2 tabs of the TUI and their
// views), and one top bar with the time window and live state.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Breadcrumbs, Button, Select, Sidebar, Text, Tooltip, useKumoToastManager, useSidebar } from "@cloudflare/kumo";
import {
  ArrowsClockwiseIcon, BugIcon, ChartLineIcon, GaugeIcon, GraphIcon, HeartbeatIcon, KeyboardIcon, ListMagnifyingGlassIcon,
  MoonIcon, PauseIcon, PlayIcon, SignOutIcon, SunIcon,
} from "@phosphor-icons/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { matchPath, useLocation, useNavigate } from "react-router-dom";
import { send, type Session } from "@/api/client";
import { otel } from "@/api/otel";
import { sentry } from "@/api/sentry";
import { useTick } from "@/components/ui-bits";
import { clean } from "@/lib/clean";
import { compact } from "@/lib/format";
import { cn } from "@/lib/utils";
import { leaveMenu, REFRESH_EVENT, useKeys } from "./keys";
import { lastTelemetryView, record, screenAddress } from "./history";
import { useInterval, useSettings } from "./settings";
import { HelpDialog } from "./Help";
import { useWindow, windowsOf, type Side } from "./window";
import { Logo } from "./Logo";

function useSide(): Side {
  const { pathname } = useLocation();
  return pathname.startsWith("/telemetry") ? "telemetry" : "errors";
}

interface Crumb {
  label: string;
  href?: string;
  copy?: string;
}

const VIEWS: Record<string, string> = { overview: "Overview", traces: "Traces", logs: "Logs", metrics: "Metrics", status: "Status" };

/** The one way to see where you are and go back up, on every screen. */
function useCrumbs(side: Side): Crumb[] {
  const { pathname, search } = useLocation();
  const param = new URLSearchParams(search);
  // The address can be a crafted link: same rule as server text.
  if (side === "errors") {
    const issue = matchPath("/errors/:project/issues/:id", pathname);
    if (issue) {
      return [
        { label: "Errors" },
        { label: "Issues", href: `/errors/${issue.params.project}` },
        { label: clean(issue.params.id!).slice(0, 16), copy: clean(issue.params.id!) },
      ];
    }
    const trace = matchPath("/errors-by-trace/:id", pathname);
    if (trace) return [{ label: "Errors" }, { label: "Errors of a trace", copy: clean(trace.params.id!) }];
    return [{ label: "Errors" }, { label: "Issues" }];
  }
  const view = pathname.split("/")[2] ?? "overview";
  const name = VIEWS[view] ?? "Overview";
  const detail = matchPath("/telemetry/:view/:id", pathname);
  if (detail) {
    const id = clean(detail.params.id!);
    return [{ label: "Telemetry" }, { label: name, href: `/telemetry/${view}` }, { label: view === "traces" ? id.slice(0, 16) : id, copy: id }];
  }
  const service = view === "overview" ? param.get("service") : null;
  if (service) return [{ label: "Telemetry" }, { label: name, href: "/telemetry/overview" }, { label: clean(service) }];
  return [{ label: "Telemetry" }, { label: name }];
}

function Crumbs({ side }: { side: Side }) {
  const crumbs = useCrumbs(side);
  const last = crumbs[crumbs.length - 1]!;
  useEffect(() => {
    document.title = `${last.label} · mallard`;
  }, [last.label]);
  return (
    <Breadcrumbs className="mr-0 min-w-0 flex-1">
      {crumbs.slice(0, -1).map((c) => (
        <span key={c.label} className={cn("items-center gap-1", c.href ? "flex" : "hidden lg:flex")}>
          {c.href ? <Breadcrumbs.Link href={c.href}>{c.label}</Breadcrumbs.Link> : <Text variant="secondary">{c.label}</Text>}
          <Breadcrumbs.Separator />
        </span>
      ))}
      <Breadcrumbs.Current>{last.label}</Breadcrumbs.Current>
      {last.copy && <span className="hidden lg:flex"><Breadcrumbs.Clipboard text={last.copy} /></span>}
    </Breadcrumbs>
  );
}

/** Traces, the errors of a trace and the status page have no window of their own. */
function hasWindow(pathname: string): boolean {
  return !(
    matchPath("/telemetry/traces/:id", pathname) ||
    matchPath("/errors-by-trace/:id", pathname) ||
    pathname === "/telemetry/status"
  );
}

function WindowControl({ side }: { side: Side }) {
  const { w, setW, cycle } = useWindow(side);
  useKeys({ t: () => cycle(1), T: () => cycle(-1) });
  const items = Object.fromEntries(windowsOf(side).map((x) => [x, `Last ${x}`]));
  return (
    <div className="flex items-center gap-2">
      <Tooltip content={<span>Time window (t / T). All times are UTC.</span>} asChild>
        <span className="hidden sm:inline"><Text variant="secondary">UTC</Text></span>
      </Tooltip>
      <Select aria-label="Time window" className="w-28" value={w} onValueChange={(v) => v && setW(v as string)} onOpenChange={leaveMenu} items={items} />
    </div>
  );
}

/** Accepted records per second between two stats loads (TUI: only over 0.5s). */
function useIngestRate(accepted: number | undefined, at: number): number | null {
  const last = useRef<{ accepted: number; at: number } | null>(null);
  const [rate, setRate] = useState<number | null>(null);
  useEffect(() => {
    if (accepted === undefined) return;
    const prev = last.current;
    if (prev && at - prev.at > 500) setRate(Math.max(0, (accepted - prev.accepted) / ((at - prev.at) / 1000)));
    if (!prev || at - prev.at > 500) last.current = { accepted, at };
  }, [accepted, at]);
  return rate;
}

function LiveState({ side }: { side: Side }) {
  const infoInterval = useInterval(2000);
  const statsInterval = useInterval(5000);
  useTick(1000);
  const info = useQuery({ queryKey: ["sentry", "info"], queryFn: ({ signal }) => sentry.info(signal), refetchInterval: infoInterval, enabled: side === "errors" });
  const stats = useQuery({ queryKey: ["otel", "stats"], queryFn: ({ signal }) => otel.stats(signal), refetchInterval: statsInterval, enabled: side === "telemetry" });
  const rate = useIngestRate(stats.data?.ingest.signals.reduce((a, x) => a + x.accepted, 0), stats.dataUpdatedAt);
  const q = side === "errors" ? info : stats;
  const age = q.dataUpdatedAt ? (Date.now() - q.dataUpdatedAt) / 1000 : Infinity;
  const { paused } = useSettings();
  const state = q.isError && (!q.data || side === "errors") ? "offline" : !q.data ? "connecting" : paused ? "paused" : age > 20 ? "stale" : "live";
  const tone = { live: "bg-kumo-success", paused: "bg-kumo-fill", connecting: "bg-kumo-fill", stale: "bg-kumo-warning", offline: "bg-kumo-danger" }[state];
  const text = { live: "Live", paused: "Paused", connecting: "Connecting", stale: "Stale", offline: "Offline" }[state];

  let extra: string | null = side === "telemetry" && rate !== null ? `${compact(rate)}/s ingest` : null;
  if (side === "errors" && info.data) {
    const { queue_depth, queue_capacity } = info.data;
    if (queue_depth > 0) extra = `queue ${compact(queue_depth)}/${compact(queue_capacity)}`;
  }
  return (
    <div role="status" className="tnum flex items-center gap-2 text-xs text-kumo-subtle">
      <span className="inline-flex items-center gap-1.5 text-kumo-default">
        <span aria-hidden className={cn("size-2 rounded-full", tone)} />
        <span className="hidden sm:inline">{text}</span>
      </span>
      {q.data && Number.isFinite(age) && <span className="hidden lg:inline">updated {age < 2 ? "now" : `${Math.floor(age)}s ago`}</span>}
      {extra && <span className="hidden lg:inline">{extra}</span>}
    </div>
  );
}

function TopBar({ side }: { side: Side }) {
  const { pathname } = useLocation();
  const { paused, setPaused, setHelpOpen } = useSettings();
  const client = useQueryClient();
  const refresh = () => {
    void client.invalidateQueries();
    window.dispatchEvent(new Event(REFRESH_EVENT));
  };
  useKeys({ r: refresh, "?": () => setHelpOpen(true) });
  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b border-kumo-line bg-kumo-base px-5">
      <Sidebar.Trigger aria-label="Show or hide the navigation" />
      <Crumbs side={side} />
      <div className="ml-auto flex shrink-0 items-center gap-2">
        <LiveState side={side} />
        {hasWindow(pathname) && <WindowControl side={side} />}
        <Tooltip content={paused ? "Resume auto refresh" : "Pause auto refresh"} asChild>
          <Button variant="ghost" shape="square" aria-label={paused ? "Resume auto refresh" : "Pause auto refresh"} aria-pressed={paused} onClick={() => setPaused(!paused)} icon={paused ? <PlayIcon /> : <PauseIcon />} />
        </Tooltip>
        <Tooltip content="Refresh now (r)" asChild>
          <Button variant="ghost" shape="square" aria-label="Refresh now" onClick={refresh} icon={<ArrowsClockwiseIcon />} />
        </Tooltip>
      </div>
    </header>
  );
}

const TELEMETRY_VIEWS = [
  { key: "overview", label: "Overview", icon: GaugeIcon },
  { key: "traces", label: "Traces", icon: GraphIcon },
  { key: "logs", label: "Logs", icon: ListMagnifyingGlassIcon },
  { key: "metrics", label: "Metrics", icon: ChartLineIcon },
  { key: "status", label: "Status", icon: HeartbeatIcon },
] as const;

function Nav({ session, side }: { session: Session; side: Side }) {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const { theme, isDark, setTheme, setHelpOpen } = useSettings();
  const client = useQueryClient();
  const toasts = useKumoToastManager();
  const { state, isMobile } = useSidebar();
  const { search } = useLocation();
  useEffect(() => record(pathname, search), [pathname, search]);

  useKeys({
    F1: () => navigate(screenAddress("/errors")),
    F2: () => navigate(screenAddress(lastTelemetryView())),
    ...(side === "telemetry"
      ? Object.fromEntries(TELEMETRY_VIEWS.map((v, i) => [String(i + 1), () => navigate(screenAddress(`/telemetry/${v.key}`))]))
      : {}),
  });

  const signOut = async () => {
    try {
      await send("POST", "/web/logout");
      await client.invalidateQueries({ queryKey: ["session"] });
    } catch (e) {
      toasts.add({ title: "Could not sign out", description: (e as Error).message, variant: "error" });
    }
  };

  return (
    <Sidebar>
      <Sidebar.Header className="h-12 shrink-0">
        <a href="/" onClick={(e) => { e.preventDefault(); navigate("/errors"); }} className="flex items-center gap-2 px-1 py-0.5 font-semibold text-kumo-strong">
          <Logo className="size-6 shrink-0" />
          {state === "expanded" || isMobile ? <Text bold>mallard</Text> : null}
        </a>
      </Sidebar.Header>
      <Sidebar.Content>
        <Sidebar.Group>
          <Sidebar.GroupLabel>Errors</Sidebar.GroupLabel>
          <Sidebar.Menu>
            <Sidebar.MenuButton icon={BugIcon} active={side === "errors"} href={screenAddress("/errors")}>
              Issues
            </Sidebar.MenuButton>
          </Sidebar.Menu>
        </Sidebar.Group>
        <Sidebar.Group>
          <Sidebar.GroupLabel>Telemetry</Sidebar.GroupLabel>
          <Sidebar.Menu>
            {TELEMETRY_VIEWS.map((v) => (
              <Sidebar.MenuButton key={v.key} icon={v.icon} active={pathname.startsWith(`/telemetry/${v.key}`) || (v.key === "overview" && pathname === "/telemetry")} href={screenAddress(`/telemetry/${v.key}`)}>
                {v.label}
              </Sidebar.MenuButton>
            ))}
          </Sidebar.Menu>
        </Sidebar.Group>
      </Sidebar.Content>
      <Sidebar.Footer className="h-auto flex-col items-stretch gap-1 py-2">
        <Sidebar.Menu>
          <Sidebar.MenuButton icon={KeyboardIcon} onClick={() => setHelpOpen(true)}>Keys</Sidebar.MenuButton>
          <Sidebar.MenuButton icon={isDark ? SunIcon : MoonIcon} onClick={() => setTheme(isDark ? "light" : "dark")} aria-label={`Theme: ${theme}. Switch to ${isDark ? "light" : "dark"}`}>
            {isDark ? "Light theme" : "Dark theme"}
          </Sidebar.MenuButton>
          {session.mode === "login" && (
            <Sidebar.MenuButton icon={SignOutIcon} onClick={() => void signOut()}>Sign out</Sidebar.MenuButton>
          )}
        </Sidebar.Menu>
        {session.server_url && (state === "expanded" || isMobile) && (
          <p className="truncate px-2 pt-1 text-left text-xs text-kumo-subtle" title={session.server_url}>{session.server_url}, v{session.version}</p>
        )}
      </Sidebar.Footer>
    </Sidebar>
  );
}

export function Shell({ session, children }: { session: Session; children: ReactNode }) {
  const side = useSide();
  return (
    <Sidebar.Provider collapsible="icon" className="h-dvh">
      <Nav session={session} side={side} />
      <div className="flex h-dvh min-w-0 flex-1 flex-col">
        <TopBar side={side} />
        <main className="flex min-h-0 flex-1 flex-col overflow-hidden">{children}</main>
      </div>
      <HelpDialog />
    </Sidebar.Provider>
  );
}
