// Where Esc goes and where a tab returns to. As in the TUI, each side keeps its own
// back stack and its own screens: moving between sides or views never changes where
// "back" leads, and browser history (which also holds window and filter changes) is
// not used for it.

export type Side = "errors" | "telemetry";

interface Entry {
  pathname: string;
  search: string;
}

const stacks: Record<Side, Entry[]> = { errors: [], telemetry: [] };
const screens = new Map<string, Entry>();
let telemetryView = "/telemetry/overview";

export const sideOf = (pathname: string): Side => (pathname.startsWith("/telemetry") ? "telemetry" : "errors");

/** `/errors/web` and `/telemetry/traces` are floors of a stack, as the issue list is. */
function isList(pathname: string): boolean {
  return /^\/errors\/[^/]+$/.test(pathname) || /^\/telemetry\/[^/]+$/.test(pathname);
}

/** A screen is what a tab or view key returns to. */
function screenOf(pathname: string): string {
  return pathname.startsWith("/telemetry/") ? pathname.split("/").slice(0, 3).join("/") : "/errors";
}

/** The window belongs to the side, not to one address: it is never restored from here. */
function withoutWindow(search: string): string {
  const p = new URLSearchParams(search);
  p.delete("w");
  const s = p.toString();
  return s ? `?${s}` : "";
}

const address = (e: Entry): string => e.pathname + withoutWindow(e.search);

export function record(pathname: string, search: string): void {
  // These two only redirect.
  if (pathname === "/" || pathname === "/errors" || pathname === "/telemetry") return;
  const side = sideOf(pathname);
  const stack = stacks[side];
  const entry = { pathname, search };
  const at = stack.findIndex((e) => e.pathname === pathname);
  if (at >= 0) {
    stack.length = at + 1;
    stack[at] = entry;
  } else if (isList(pathname)) {
    stack.length = 0;
    stack.push(entry);
  } else {
    stack.push(entry);
  }
  screens.set(screenOf(pathname), entry);
  if (side === "telemetry") telemetryView = screenOf(pathname);
}

/** Where Esc goes from the top of this side's stack. */
export function backTarget(side: Side, fallback: string): string {
  const stack = stacks[side];
  const previous = stack[stack.length - 2];
  return previous ? address(previous) : fallback;
}

/** Where a view key or side nav entry returns to, with its filters. */
export function screenAddress(screen: string): string {
  const e = screens.get(screen);
  return e ? address(e) : screen;
}

/** Where F2 returns to: the last telemetry view. */
export const lastTelemetryView = (): string => telemetryView;

/** For tests. */
export function reset(): void {
  stacks.errors.length = 0;
  stacks.telemetry.length = 0;
  screens.clear();
  telemetryView = "/telemetry/overview";
}
