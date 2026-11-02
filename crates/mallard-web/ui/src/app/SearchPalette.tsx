// Cmd-K: paste any id and land on it, or type a name and pick the app.
import { useEffect, useMemo, useRef, useState } from "react";
import { Button, CommandPalette, Tooltip } from "@cloudflare/kumo";
import { AppWindowIcon, BugIcon, GraphIcon, MagnifyingGlassIcon, WarningIcon } from "@phosphor-icons/react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { apps as appsApi } from "@/api/app";
import { ApiError } from "@/api/client";
import type { FindMatch } from "@/api/types";
import { parseFindId } from "@/lib/query";
import { compact } from "@/lib/format";
import { useScope } from "./window";

const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform);
export const SEARCH_KEY = IS_MAC ? "⌘K" : "Ctrl K";

interface Row {
  id: string;
  title: string;
  sub: string;
  icon: React.ReactNode;
  go: () => void;
}

interface Group {
  id: string;
  label: string;
  items: Row[];
}

const issueAddress = (project: string, issue: string, event?: string) =>
  `/errors/${encodeURIComponent(project)}/issues/${encodeURIComponent(issue)}${event ? `?event=${encodeURIComponent(event)}` : ""}`;

function addressOf(m: FindMatch): string {
  switch (m.kind) {
    case "trace":
      return `/telemetry/traces/${m.trace_id}?hint=${encodeURIComponent(m.time)}`;
    case "span":
      return `/telemetry/traces/${m.trace_id}?hint=${encodeURIComponent(m.time)}&span=${m.span_id}`;
    case "event":
      return issueAddress(m.project, m.issue_id, m.event_id);
    case "issue":
      return issueAddress(m.project, m.issue_id);
  }
}

function describe(m: FindMatch): { title: string; sub: string; icon: React.ReactNode } {
  switch (m.kind) {
    case "trace":
      return { title: `Trace ${m.trace_id}`, sub: `${compact(m.spans)} spans, ${compact(m.errors)} errors, ${compact(m.logs)} logs`, icon: <GraphIcon /> };
    case "span":
      return { title: m.name, sub: `Span in ${m.service}`, icon: <GraphIcon /> };
    case "event":
      return { title: m.title, sub: `Event in ${m.project}`, icon: <BugIcon /> };
    case "issue":
      return { title: m.title, sub: `Issue in ${m.project}, ${m.status}, ${compact(m.count)} events`, icon: <BugIcon /> };
  }
}

const allowedKeys = (e: KeyboardEvent) => (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k";

export function SearchPalette() {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const navigate = useNavigate();
  const { apps, setApp, windows } = useScope();

  // A modified key, so it works while typing and with single-key shortcuts off.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!allowedKeys(e)) return;
      e.preventDefault();
      e.stopPropagation();
      setOpen((o) => !o);
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, []);

  const setOpenAndClear = (next: boolean) => {
    setOpen(next);
    if (!next) setText("");
  };

  const id = parseFindId(text);
  const since = windows[windows.length - 1]!;
  const found = useQuery({
    queryKey: ["find", id, since],
    enabled: open && !!id,
    retry: false,
    queryFn: ({ signal }) => appsApi.find(id!, since, signal),
  });
  const matches = useMemo(() => (id && found.data ? found.data.matches : []), [id, found.data]);

  const close = () => setOpenAndClear(false);
  const groups = useMemo<Group[]>(() => {
    const out: Group[] = [];
    if (matches.length) {
      out.push({
        id: "found",
        label: "Found",
        items: matches.map((m, i) => ({
          ...describe(m),
          id: `${m.kind}-${i}`,
          go: () => {
            navigate(addressOf(m));
            close();
          },
        })),
      });
    }
    const needle = text.trim().toLowerCase();
    const named = id ? [] : apps.filter((a) => a.slug.includes(needle) || a.name.toLowerCase().includes(needle));
    if (named.length) {
      out.push({
        id: "apps",
        label: "Apps",
        items: named.slice(0, 50).map((a) => ({
          id: a.slug,
          title: a.name,
          sub: a.slug === a.name ? "" : a.slug,
          icon: <AppWindowIcon />,
          go: () => {
            setApp(a.slug);
            close();
          },
        })),
      });
    }
    return out;
    // `close` only clears local state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [matches, apps, text, id, navigate, setApp]);

  // One thing found for a whole id: land on it, that is why it was pasted. With a warning a side
  // could not be read, so there may be more: show both, as the TUI does.
  const landed = useRef<string | null>(null);
  useEffect(() => {
    if (!open) landed.current = null;
    if (open && id && found.data && !found.isFetching && matches.length === 1 && !found.data.warnings.length && landed.current !== id) {
      landed.current = id;
      navigate(addressOf(matches[0]!));
      setOpenAndClear(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, id, found.data, found.isFetching, matches]);

  const notFound = found.error instanceof ApiError && found.error.kind === "not_found";
  const warnings = id && found.data ? found.data.warnings : [];
  const emptyText = !id
    ? "No app has this name. Paste a trace, event, issue or span id to open it."
    : found.isFetching
      ? "Looking…"
      : notFound
        ? `Nothing found for ${id} in the last ${since}.`
        : found.error
          ? `Could not look this up: ${found.error.message}`
          : "";

  return (
    <>
      <Tooltip content={`Search apps and ids (${SEARCH_KEY})`} asChild>
        <Button variant="ghost" shape="square" aria-label="Search apps and ids" aria-keyshortcuts="Meta+K Control+K" onClick={() => setOpen(true)} icon={<MagnifyingGlassIcon />} />
      </Tooltip>
      <CommandPalette.Root<Group, Row>
        open={open}
        onOpenChange={setOpenAndClear}
        items={groups}
        value={text}
        onValueChange={setText}
        itemToStringValue={(g) => g.label}
        getSelectableItems={(gs) => gs.flatMap((g) => g.items)}
        onSelect={(row) => row.go()}
      >
        <CommandPalette.Input aria-label="App name, or a trace, event, issue or span id" placeholder="App name, or paste an id" spellCheck={false} autoComplete="off" />
        <CommandPalette.List>
          <CommandPalette.Results>
            {(g: Group) => (
              <CommandPalette.Group key={g.id} items={g.items}>
                <CommandPalette.GroupLabel>{g.label}</CommandPalette.GroupLabel>
                <CommandPalette.Items>
                  {(row: Row) => (
                    <CommandPalette.Item key={row.id} value={row} onClick={row.go}>
                      <span className="flex min-w-0 items-center gap-3">
                        <span aria-hidden className="shrink-0 text-kumo-subtle">{row.icon}</span>
                        <span className="min-w-0 truncate">{row.title}</span>
                        {row.sub && <span className="min-w-0 shrink-0 truncate text-kumo-subtle">{row.sub}</span>}
                      </span>
                    </CommandPalette.Item>
                  )}
                </CommandPalette.Items>
              </CommandPalette.Group>
            )}
          </CommandPalette.Results>
          <CommandPalette.Empty>{emptyText}</CommandPalette.Empty>
        </CommandPalette.List>
        {warnings.length > 0 && (
          <CommandPalette.Footer>
            <span className="flex items-center gap-2 text-kumo-warning"><WarningIcon aria-hidden />Not everything could be read: {warnings.join("; ")}</span>
          </CommandPalette.Footer>
        )}
      </CommandPalette.Root>
    </>
  );
}
