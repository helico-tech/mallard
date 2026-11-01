// Same rules as `mallard_term::text` (ADR 0006): every control and bidi
// character from the server becomes a visible escape and never reaches the
// screen. React already escapes HTML; this stops spoofing and hidden text.

function isUnsafe(cp: number): boolean {
  return (
    cp < 0x20 || (cp >= 0x7f && cp <= 0x9f) || cp === 0x200e || cp === 0x200f ||
    (cp >= 0x202a && cp <= 0x202e) || (cp >= 0x2066 && cp <= 0x2069)
  );
}

function escape(cp: number): string {
  return `\\u{${cp.toString(16)}}`;
}

/** `text::clean`: one line. Newlines become spaces, tabs four spaces. */
export function clean(s: string): string {
  let out = "";
  for (const ch of s) {
    const cp = ch.codePointAt(0)!;
    if (ch === "\n" || ch === "\r") out += " ";
    else if (ch === "\t") out += "    ";
    else out += isUnsafe(cp) ? escape(cp) : ch;
  }
  return out;
}

/** `text::clean_text`: keeps newlines, drops `\r`. */
export function cleanText(s: string): string {
  let out = "";
  for (const ch of s) {
    const cp = ch.codePointAt(0)!;
    if (ch === "\n") out += ch;
    else if (ch === "\r") continue;
    else if (ch === "\t") out += "    ";
    else out += isUnsafe(cp) ? escape(cp) : ch;
  }
  return out;
}

/** Cleans every string and object key of a response, like the errors tab of the TUI. */
export function cleanDeep<T>(v: T): T {
  if (typeof v === "string") return cleanText(v) as T;
  if (Array.isArray(v)) return v.map(cleanDeep) as T;
  if (v && typeof v === "object") {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [clean(k), cleanDeep(x)])) as T;
  }
  return v;
}
