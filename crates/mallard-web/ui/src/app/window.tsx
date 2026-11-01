// The time window of each side. The URL holds it (shareable); a side keeps its
// window while you move around, and each side has its own, as in the TUI.
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { DEFAULT_ERROR_WINDOW, DEFAULT_TELEMETRY_WINDOW, ERROR_WINDOWS, TELEMETRY_WINDOWS } from "@/lib/domain";

export type { Side } from "./history";
import type { Side } from "./history";

export const windowsOf = (side: Side): readonly string[] =>
  side === "errors" ? ERROR_WINDOWS.map((w) => w.w) : TELEMETRY_WINDOWS;

const Ctx = createContext<{
  current: Record<Side, string>;
  set: (side: Side, w: string) => void;
} | null>(null);

export function WindowProvider({ children }: { children: ReactNode }) {
  const [current, setCurrent] = useState<Record<Side, string>>({
    errors: DEFAULT_ERROR_WINDOW,
    telemetry: DEFAULT_TELEMETRY_WINDOW,
  });
  const set = useCallback((side: Side, w: string) => setCurrent((c) => (c[side] === w ? c : { ...c, [side]: w })), []);
  const value = useMemo(() => ({ current, set }), [current, set]);
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useWindow(side: Side): { w: string; setW: (w: string) => void; cycle: (dir: 1 | -1) => void } {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useWindow outside WindowProvider");
  const [params, setParams] = useSearchParams();
  const fromUrl = params.get("w");
  const options = windowsOf(side);
  const valid = fromUrl && options.includes(fromUrl) ? fromUrl : null;
  const w = valid ?? ctx.current[side];
  const { set } = ctx;

  useEffect(() => {
    if (valid) set(side, valid);
    else
      setParams(
        (p) => {
          p.set("w", w);
          return p;
        },
        { replace: true },
      );
  }, [valid, side, set, setParams, w]);

  const rendered = useRef(w);
  rendered.current = w;
  const setW = useCallback(
    (next: string) => {
      set(side, next);
      setParams((p) => {
        p.set("w", next);
        return p;
      });
    },
    [set, side, setParams],
  );
  const cycle = useCallback(
    (dir: 1 | -1) => {
      // Two quick presses must not start from a stale value: the address bar
      // changes at once, while `w` waits for the next render.
      const inUrl = new URLSearchParams(window.location.search).get("w");
      const from = inUrl && options.includes(inUrl) ? inUrl : rendered.current;
      const i = options.indexOf(from);
      setW(options[(i + dir + options.length) % options.length]!);
    },
    [options, setW],
  );
  return { w, setW, cycle };
}

/** Sets one search param; empty removes it. */
export function useParam(name: string, fallback = ""): [string, (v: string, replace?: boolean) => void] {
  const [params, setParams] = useSearchParams();
  const value = params.get(name) ?? fallback;
  const set = useCallback(
    (v: string, replace = false) => {
      setParams(
        (p) => {
          if (v) p.set(name, v);
          else p.delete(name);
          return p;
        },
        { replace },
      );
    },
    [name, setParams],
  );
  return [value, set];
}
