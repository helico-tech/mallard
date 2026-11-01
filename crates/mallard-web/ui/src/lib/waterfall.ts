// Span tree and bar geometry (`odl-cli/src/waterfall.rs`).

export interface TreeSpan {
  span_id: string;
  parent_span_id?: string | null;
  start_time: string;
  duration_ns: number;
}

export interface Row<S> {
  span: S;
  depth: number;
  hasChildren: boolean;
  /** Spans below this one. */
  descendants: number;
  /** Parent id set but that span was not received. */
  orphan: boolean;
  offsetMs: number;
  durationMs: number;
}

export interface Tree<S> {
  rows: Row<S>[];
  totalMs: number;
  startMs: number;
}

/** Microseconds matter here and `Date` keeps only milliseconds. */
export function epochMicros(t: string): number {
  const base = Date.parse(t);
  const m = /\.(\d+)/.exec(t);
  const frac = m ? Number(`0.${m[1]}`) * 1000 : 0;
  return (Number.isNaN(base) ? 0 : Math.floor(base / 1000) * 1000) * 1000 + frac * 1000;
}

const order = <S extends TreeSpan>(a: S, b: S) =>
  epochMicros(a.start_time) - epochMicros(b.start_time) || (a.span_id < b.span_id ? -1 : a.span_id > b.span_id ? 1 : 0);

/** Depth first, children by start time then id. A span whose parent is
 *  missing becomes a root; spans in a cycle are appended as roots. */
export function buildTree<S extends TreeSpan>(spans: S[], collapsed: ReadonlySet<string> = new Set()): Tree<S> {
  if (spans.length === 0) return { rows: [], totalMs: 0, startMs: 0 };
  const ids = new Set(spans.map((s) => s.span_id));
  const children = new Map<string, S[]>();
  const roots: S[] = [];
  for (const s of spans) {
    if (s.parent_span_id && ids.has(s.parent_span_id) && s.parent_span_id !== s.span_id) {
      const list = children.get(s.parent_span_id) ?? [];
      list.push(s);
      children.set(s.parent_span_id, list);
    } else {
      roots.push(s);
    }
  }
  for (const list of children.values()) list.sort(order);
  roots.sort(order);

  let startUs = Infinity;
  let endUs = -Infinity;
  for (const s of spans) {
    const st = epochMicros(s.start_time);
    startUs = Math.min(startUs, st);
    endUs = Math.max(endUs, st + s.duration_ns / 1000);
  }

  // Spans below each span, children before parents: a trace can be deeper than the call stack.
  const below = new Map<string, number>();
  {
    const order: S[] = [];
    const parentOf = new Map<string, string>();
    const visited = new Set<string>();
    const walk = (start: S) => {
      const todo = [start];
      while (todo.length) {
        const x = todo.pop()!;
        if (visited.has(x.span_id)) continue;
        visited.add(x.span_id);
        order.push(x);
        for (const c of children.get(x.span_id) ?? []) {
          if (!visited.has(c.span_id)) {
            parentOf.set(c.span_id, x.span_id);
            todo.push(c);
          }
        }
      }
    };
    roots.forEach(walk);
    spans.forEach(walk); // Cycles have no root.
    for (let i = order.length - 1; i >= 0; i--) {
      const id = order[i]!.span_id;
      const p = parentOf.get(id);
      if (p) below.set(p, (below.get(p) ?? 0) + (below.get(id) ?? 0) + 1);
    }
  }

  const seen = new Set<string>();
  const rows: Row<S>[] = [];
  const visit = (root: S) => {
    const stack: { s: S; depth: number }[] = [{ s: root, depth: 0 }];
    while (stack.length) {
      const { s, depth } = stack.pop()!;
      if (seen.has(s.span_id)) continue;
      seen.add(s.span_id);
      const kids = children.get(s.span_id) ?? [];
      rows.push({
        span: s,
        depth,
        hasChildren: kids.length > 0,
        descendants: below.get(s.span_id) ?? 0,
        orphan: !!s.parent_span_id && !ids.has(s.parent_span_id),
        offsetMs: (epochMicros(s.start_time) - startUs) / 1000,
        durationMs: s.duration_ns / 1e6,
      });
      if (collapsed.has(s.span_id)) {
        for (const id of descendantIds(s.span_id, children)) seen.add(id);
        continue;
      }
      for (let i = kids.length - 1; i >= 0; i--) {
        stack.push({ s: kids[i]!, depth: depth + 1 });
      }
    }
  };
  roots.forEach((r) => visit(r));
  // Cycles have no root: append them.
  const rest = spans.filter((s) => !seen.has(s.span_id)).sort(order);
  rest.forEach((s) => visit(s));

  return { rows, totalMs: (endUs - startUs) / 1000, startMs: startUs / 1000 };
}

function descendantIds<S extends TreeSpan>(id: string, children: Map<string, S[]>): string[] {
  const out: string[] = [];
  const stack = [id];
  const seen = new Set([id]);
  while (stack.length) {
    for (const c of children.get(stack.pop()!) ?? []) {
      if (seen.has(c.span_id)) continue;
      seen.add(c.span_id);
      out.push(c.span_id);
      stack.push(c.span_id);
    }
  }
  return out;
}

/** Bar position in percent; at least 0.4% wide so a tiny span stays visible. */
export function barGeometry(offsetMs: number, durationMs: number, totalMs: number): { left: number; width: number } {
  if (totalMs <= 0) return { left: 0, width: 100 };
  const left = Math.min(100, Math.max(0, (offsetMs / totalMs) * 100));
  const width = Math.max(0.4, (durationMs / totalMs) * 100);
  return { left: Math.min(left, 99.6), width: Math.min(width, 100 - Math.min(left, 99.6)) };
}
