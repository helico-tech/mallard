// The app and the time window: one of each for the whole UI (ADR 0028). The address
// holds them, so a link shows the same thing. An address without them keeps the
// current ones, and `?app=` (empty) means All apps.
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, type ReactNode } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { matchPath, useLocation, useNavigate, useSearchParams, type NavigateFunction } from "react-router-dom";
import { apps as appsApi } from "@/api/app";
import { otel } from "@/api/otel";
import { sentry } from "@/api/sentry";
import type { App } from "@/api/types";
import { DEFAULT_WINDOW, windowsWithin } from "@/lib/domain";
import { forgetApp } from "./history";
import { useInterval } from "./settings";

export type { Side } from "./history";

interface Scope {
  /** The slug, or "" for All apps. May not be in `apps`: an app with no data in this window. */
  app: string;
  info: App | undefined;
  /** Every app with data in this window, and every project. */
  apps: App[];
  /** What telemetry reads are limited to; null is every service. */
  services: string[] | null;
  /** The project's slug, when the app has one. */
  project: string | null;
  /** The list is not there yet, so a selected app cannot be scoped yet. */
  loading: boolean;
  /** The list of apps has answered at least once. */
  loaded: boolean;
  error: unknown;
  warnings: string[];
  setApp: (slug: string) => void;
  w: string;
  windows: string[];
  setW: (w: string) => void;
  cycle: (dir: 1 | -1) => void;
}

const Ctx = createContext<Scope | null>(null);

/** Changes the address as it is now. A render can lag behind it (the address changes
 *  at once, the screen a moment later), and starting from that render's parameters
 *  would silently undo a change made in between, such as two quick keys. */
export function writeParams(navigate: NavigateFunction, change: (p: URLSearchParams) => void, replace = false): void {
  const p = new URLSearchParams(window.location.search);
  change(p);
  navigate({ pathname: window.location.pathname, search: `?${p.toString()}` }, { replace });
}

export function ScopeProvider({ children }: { children: ReactNode }) {
  const { pathname, search } = useLocation();
  const navigate = useNavigate();
  const [params] = useSearchParams();

  // Limits of both sides; the shell's live state keeps these two fresh.
  const info = useQuery({ queryKey: ["sentry", "info"], queryFn: ({ signal }) => sentry.info(signal) });
  const stats = useQuery({ queryKey: ["otel", "stats"], queryFn: ({ signal }) => otel.stats(signal) });
  const infoLimit = info.data?.max_window_secs;
  const statsLimit = stats.data?.capabilities?.max_window_secs;
  const windows = useMemo(() => windowsWithin([infoLimit, statsLimit]), [infoLimit, statsLimit]);

  const lastW = useRef<string>(DEFAULT_WINDOW);
  const fromUrl = params.get("w");
  const w = fromUrl && windows.includes(fromUrl) ? fromUrl : windows.includes(lastW.current) ? lastW.current : windows[windows.length - 1]!;

  const refetchInterval = useInterval(30000);
  const list = useQuery({
    queryKey: ["apps", w],
    queryFn: ({ signal }) => appsApi.list(w, signal),
    refetchInterval,
    placeholderData: keepPreviousData,
  });
  const apps = useMemo(() => list.data?.apps ?? [], [list.data]);

  // On the errors side the project is in the path; that wins.
  const pathProject = matchPath("/errors/:project/*", pathname)?.params.project ?? null;
  const lastApp = useRef("");
  const shownApp = useRef<string | null>(null);
  const asked = pathProject ?? params.get("app") ?? lastApp.current;
  const found = asked ? apps.find((a) => a.slug === asked || String(a.project ?? "") === asked) : undefined;
  const app = found?.slug ?? asked;

  useEffect(() => {
    lastW.current = w;
    lastApp.current = app;
    if (shownApp.current !== null && shownApp.current !== app) forgetApp();
    shownApp.current = app;
    // The address has moved on since this render: writing now would undo that. The
    // render of the new address comes through here too.
    if (window.location.pathname !== pathname || window.location.search !== search) return;
    const next = new URLSearchParams(search);
    next.set("w", w);
    if (app && !pathProject) next.set("app", app);
    else next.delete("app");
    const before = new URLSearchParams(search);
    const same = before.get("w") === next.get("w") && before.get("app") === next.get("app");
    if (!same) navigate({ pathname, search: `?${next.toString()}` }, { replace: true });
  }, [w, app, pathProject, pathname, search, navigate]);

  const setW = useCallback(
    (next: string) => {
      // At once: the address can move on (F1) before this window has been drawn.
      lastW.current = next;
      writeParams(navigate, (p) => p.set("w", next));
    },
    [navigate],
  );
  const cycle = useCallback(
    (dir: 1 | -1) => {
      // Two quick presses must not start from a stale value: the address bar
      // changes at once, while `w` waits for the next render.
      const inUrl = new URLSearchParams(window.location.search).get("w");
      const from = inUrl && windows.includes(inUrl) ? inUrl : w;
      setW(windows[(windows.indexOf(from) + dir + windows.length) % windows.length]!);
    },
    [windows, w, setW],
  );

  const setApp = useCallback(
    (slug: string) => {
      lastApp.current = slug;
      const next = new URLSearchParams(window.location.search);
      if (pathname.startsWith("/errors")) {
        // An issue or trace belongs to the old app; the list of the new one has the filters.
        const target = apps.find((a) => a.slug === slug);
        next.delete("app");
        next.delete("section");
        next.delete("event");
        if (target && target.project !== null && target.project !== undefined) {
          navigate({ pathname: `/errors/${encodeURIComponent(slug)}`, search: `?${next.toString()}` });
        } else {
          next.set("app", slug);
          navigate({ pathname: "/errors", search: `?${next.toString()}` });
        }
        return;
      }
      next.set("app", slug);
      const trace = matchPath("/telemetry/traces/:id", pathname);
      navigate({ pathname: trace ? "/telemetry/traces" : pathname, search: `?${next.toString()}` });
    },
    [apps, navigate, pathname],
  );

  const hasProject = found ? found.project !== null && found.project !== undefined : false;
  const value = useMemo<Scope>(
    () => ({
      app,
      info: found,
      apps,
      services: app ? (found?.services ?? []) : null,
      project: hasProject ? app : null,
      loading: !!app && list.isPending,
      loaded: !!list.data,
      error: list.error,
      warnings: list.data?.warnings ?? [],
      setApp,
      w,
      windows,
      setW,
      cycle,
    }),
    [app, found, apps, hasProject, list.isPending, list.error, list.data, setApp, w, windows, setW, cycle],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useScope(): Scope {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useScope outside ScopeProvider");
  return ctx;
}

export function useWindow(): Pick<Scope, "w" | "windows" | "setW" | "cycle"> {
  return useScope();
}

/** Sets one search param; empty removes it. */
export function useParam(name: string, fallback = ""): [string, (v: string, replace?: boolean) => void] {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const value = params.get(name) ?? fallback;
  const set = useCallback(
    (v: string, replace = false) => {
      writeParams(navigate, (p) => {
        if (v) p.set(name, v);
        else p.delete(name);
      }, replace);
    },
    [name, navigate],
  );
  return [value, set];
}
