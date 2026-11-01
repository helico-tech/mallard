// The filter bar's suggestions, without the screen: which token the caret is in, and
// what accepting a suggestion does to the text.

export interface Token {
  start: number;
  end: number;
  /** The key of `key=value`, once there is an `=`. */
  key: string | null;
  /** The value typed so far, or the word when there is no `=`. */
  text: string;
}

export interface Suggestion {
  label: string;
  /** What replaces the token. `key=` keeps the list open for the values. */
  insert: string;
  count?: number;
  hint?: string;
}

/** The token under the caret. Spaces inside `"double quotes"` do not end a token. */
export function tokenAt(text: string, caret: number): Token {
  let start = 0;
  let quoted = false;
  for (let i = 0; i < caret && i < text.length; i++) {
    if (text[i] === '"') quoted = !quoted;
    else if (text[i] === " " && !quoted) start = i + 1;
  }
  let end = caret;
  let inside = quoted;
  while (end < text.length && (inside || text[end] !== " ")) {
    if (text[end] === '"') inside = !inside;
    end++;
  }
  const raw = text.slice(start, Math.min(caret, end));
  const eq = raw.indexOf("=");
  if (eq <= 0) return { start, end, key: null, text: raw.replaceAll('"', "") };
  return { start, end, key: raw.slice(0, eq), text: raw.slice(eq + 1).replaceAll('"', "") };
}

/** A value with a space keeps it in quotes; the same way the filter language reads it. */
export function quoteValue(value: string): string {
  return /[\s"]/.test(value) ? `"${value.replaceAll('"', "")}"` : value;
}

/** The new text and caret. A finished token gets a space, so the next one can start. */
export function accept(text: string, token: Token, insert: string): { text: string; caret: number; more: boolean } {
  const more = insert.endsWith("=");
  const after = text.slice(token.end).replace(/^ +/, "");
  const head = text.slice(0, token.start) + insert;
  const glue = more ? "" : " ";
  return { text: head + glue + after, caret: head.length + glue.length, more };
}
