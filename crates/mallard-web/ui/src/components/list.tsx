// A list you drive like the TUI's: one selected row, kept by id across refreshes.
import { useCallback, useEffect, useRef, useState } from "react";
import { useKeys, type KeyMap } from "@/app/keys";

/** The row a list had selected, so coming back from a detail lands on it. */
const lastSelected = new Map<string, string>();

export function useListNav<T>(opts: {
  /** Which list, for coming back to the same row. */
  listKey: string;
  items: T[];
  idOf: (item: T) => string;
  onOpen?: (item: T) => void;
  pageSize?: number;
  keys?: KeyMap;
  /** Off while another pane drives the keys. */
  enabled?: boolean;
}) {
  const { listKey, items, idOf, onOpen, pageSize = 10, keys } = opts;
  const [selectedId, setSelectedId] = useState<string | null>(() => lastSelected.get(listKey) ?? null);
  const lastIndex = useRef(0);

  let index = selectedId === null ? -1 : items.findIndex((i) => idOf(i) === selectedId);
  if (index < 0 && items.length) index = Math.min(lastIndex.current, items.length - 1);
  if (index >= 0) lastIndex.current = index;
  const selected = index >= 0 ? items[index] : undefined;
  const currentId = selected ? idOf(selected) : null;

  const select = useCallback(
    (id: string | null) => {
      setSelectedId(id);
      if (id) lastSelected.set(listKey, id);
      requestAnimationFrame(() => {
        if (id) document.querySelector(`[data-row-id="${CSS.escape(id)}"]`)?.scrollIntoView({ block: "nearest" });
      });
    },
    [listKey],
  );

  const go = (i: number) => {
    if (!items.length) return;
    const next = items[Math.max(0, Math.min(items.length - 1, i))]!;
    select(idOf(next));
  };

  // Coming back to a list lands on its row. A fresh list stays at the top.
  const restoring = useRef(lastSelected.has(listKey));
  useEffect(() => {
    if (restoring.current && currentId) {
      restoring.current = false;
      document.querySelector(`[data-row-id="${CSS.escape(currentId)}"]`)?.scrollIntoView({ block: "nearest" });
    }
  }, [currentId]);

  const on = opts.enabled !== false;
  const nav: KeyMap = on
    ? {
        j: () => go(index + 1),
        k: () => go(index - 1),
        ArrowDown: () => go(index + 1),
        ArrowUp: () => go(index - 1),
        PageDown: () => go(index + pageSize),
        PageUp: () => go(index - pageSize),
        g: () => go(0),
        Home: () => go(0),
        G: () => go(items.length - 1),
        End: () => go(items.length - 1),
        ...(onOpen ? { Enter: () => selected && onOpen(selected) } : {}),
      }
    : {};
  useKeys({ ...nav, ...keys });

  return { selected, selectedId: currentId, select, index };
}
