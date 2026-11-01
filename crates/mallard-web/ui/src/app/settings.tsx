// Viewer preferences: theme, auto refresh, single-key shortcuts. Stored per
// browser; the app works the same if storage is blocked.
import { ChartPalette } from "@cloudflare/kumo";
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";

import { SERIES_COUNT } from "@/lib/domain";

export type Theme = "system" | "light" | "dark";

function load(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Preferences are a convenience.
  }
}

const systemDark = () => window.matchMedia("(prefers-color-scheme: dark)").matches;

function applyTheme(theme: Theme): boolean {
  const dark = theme === "dark" || (theme === "system" && systemDark());
  const root = document.documentElement;
  root.dataset.mode = dark ? "dark" : "light";
  // SVG marks read the design system's chart palette through variables.
  for (let i = 0; i < SERIES_COUNT; i++) root.style.setProperty(`--series-${i + 1}`, ChartPalette.categorical(i, dark));
  root.style.setProperty("--chart-attention", ChartPalette.semantic("Attention", dark));
  root.style.setProperty("--chart-warning", ChartPalette.semantic("Warning", dark));
  root.style.setProperty("--chart-neutral", ChartPalette.semantic("Neutral", dark));
  return dark;
}

interface Settings {
  theme: Theme;
  isDark: boolean;
  setTheme: (t: Theme) => void;
  paused: boolean;
  setPaused: (p: boolean) => void;
  shortcuts: boolean;
  setShortcuts: (on: boolean) => void;
  helpOpen: boolean;
  setHelpOpen: (open: boolean) => void;
}

const Ctx = createContext<Settings | null>(null);

export function SettingsProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(() => {
    const t = load("mallard.theme");
    return t === "light" || t === "dark" ? t : "system";
  });
  const [isDark, setIsDark] = useState(() => applyTheme(theme));
  const [paused, setPaused] = useState(false);
  const [shortcuts, setShortcutsState] = useState(() => load("mallard.shortcuts") !== "off");
  const [helpOpen, setHelpOpen] = useState(false);

  useEffect(() => {
    setIsDark(applyTheme(theme));
    if (theme !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => setIsDark(applyTheme("system"));
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [theme]);

  const setTheme = useCallback((t: Theme) => {
    store("mallard.theme", t);
    setThemeState(t);
  }, []);
  const setShortcuts = useCallback((on: boolean) => {
    store("mallard.shortcuts", on ? "on" : "off");
    setShortcutsState(on);
  }, []);

  const value = useMemo(
    () => ({ theme, isDark, setTheme, paused, setPaused, shortcuts, setShortcuts, helpOpen, setHelpOpen }),
    [theme, isDark, setTheme, paused, shortcuts, setShortcuts, helpOpen],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useSettings(): Settings {
  const s = useContext(Ctx);
  if (!s) throw new Error("useSettings outside SettingsProvider");
  return s;
}

/** A refetch interval that respects the pause button. */
export function useInterval(ms: number): number | false {
  return useSettings().paused ? false : ms;
}
