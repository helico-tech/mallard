// One key map (ADR 0006): the same keys as the TUI. Screens add layers; the
// newest layer wins. Keys never fire while typing, or with a popup open, and
// the viewer can turn them off (WCAG 2.1.4).
import { useEffect, useRef } from "react";
import { useSettings } from "./settings";

/** `r`: for screens that hold their own data rather than a query. */
export const REFRESH_EVENT = "mallard:refresh";

export type KeyMap = Record<string, (e: KeyboardEvent) => void>;

const layers: { current: KeyMap }[] = [];

function isTyping(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  return el.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName);
}

/** Open and on screen. A toast is a dialog too, inside its region, and a closed
 *  menu keeps an empty listbox mounted: neither may take the keys away. */
function popupOpen(): boolean {
  return [...document.querySelectorAll('[role="dialog"],[role="alertdialog"],[role="listbox"],[role="menu"]')].some(
    (e) => !e.closest('[role="region"]') && e.getBoundingClientRect().width > 0,
  );
}

/** The buttons, links, menus and tabs that need Enter and the arrows themselves. */
function isControl(el: EventTarget | null): boolean {
  return el instanceof HTMLElement && !!el.closest("button,a,[role='tab'],[role='button'],[role='combobox'],summary");
}

function needsArrows(el: EventTarget | null): boolean {
  return el instanceof HTMLElement && !!el.closest("[role='tab'],[role='combobox'],[role='separator']");
}

/** A closed menu gives the keys back: j and Enter go to the list, not the menu.
 *  The menu returns focus to itself after its close animation, so wait that out. */
export function leaveMenu(open: boolean): void {
  if (open) return;
  const leave = (el: EventTarget | null) => {
    if (el instanceof HTMLElement && el.getAttribute("role") === "combobox") el.blur();
  };
  const onFocus = (e: FocusEvent) => leave(e.target);
  document.addEventListener("focusin", onFocus);
  setTimeout(() => document.removeEventListener("focusin", onFocus), 400);
  leave(document.activeElement);
}

let enabled = true;

function onKeyDown(e: KeyboardEvent) {
  if (e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return;
  if (isTyping(e.target) || popupOpen()) return;
  const isF = /^F\d+$/.test(e.key);
  if (!enabled && !isF) return;
  if ((e.key === "Enter" || e.key === " ") && isControl(e.target)) return;
  if (e.key.startsWith("Arrow") && needsArrows(e.target)) return;
  for (let i = layers.length - 1; i >= 0; i--) {
    const fn = layers[i]!.current[e.key];
    if (fn) {
      e.preventDefault();
      e.stopPropagation();
      fn(e);
      return;
    }
  }
}

let installed = false;

/** Add a layer for as long as the component is mounted. */
export function useKeys(map: KeyMap): void {
  const { shortcuts } = useSettings();
  enabled = shortcuts;
  const ref = useRef(map);
  ref.current = map;
  useEffect(() => {
    if (!installed) {
      window.addEventListener("keydown", onKeyDown, { capture: true });
      installed = true;
    }
    const layer = ref;
    layers.push(layer);
    return () => {
      layers.splice(layers.indexOf(layer), 1);
    };
  }, []);
}

/** Shown in the help, same words as `mallard_term::tui::keys::SHARED`. */
export const SHARED_KEYS: [string, string][] = [
  ["↑ ↓  j k  PgUp PgDn  g G", "move"],
  ["Enter", "open"],
  ["Esc  q", "back"],
  ["1–9", "the tabs of this screen"],
  ["/", "filter or search"],
  ["t  T", "next / previous time window"],
  ["r", "refresh now"],
  ["x", "the same trace on the other tab"],
  ["?", "help"],
  ["F1  F2", "Errors / Telemetry"],
];

export const SCREEN_KEYS: { title: string; keys: [string, string][] }[] = [
  {
    title: "Errors, issue list",
    keys: [
      ["s", "next sort"],
      ["p", "pick a project"],
      ["h l  ← →", "previous / next status"],
      ["R  I  U", "resolve / ignore / unresolve"],
    ],
  },
  {
    title: "Errors, issue",
    keys: [
      ["[  ]", "older / newer event"],
      ["a", "in-app frames only"],
      ["w", "raw frames"],
      ["R  I  U", "resolve / ignore / unresolve"],
      ["h l  ← →", "previous / next section"],
    ],
  },
  {
    title: "Telemetry, traces",
    keys: [
      ["E", "only traces with errors"],
      ["c", "clear the filter"],
      ["l", "logs of the trace"],
    ],
  },
  {
    title: "Telemetry, logs",
    keys: [
      ["s", "next severity"],
      ["Space", "follow / pause"],
      ["Enter", "open the record"],
      ["o", "open the trace"],
      ["c", "clear the filter"],
    ],
  },
  {
    title: "Telemetry, metrics",
    keys: [
      ["a  A", "next / previous aggregation"],
      ["b", "group by"],
    ],
  },
];
