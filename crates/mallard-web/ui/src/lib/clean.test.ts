import { describe, expect, it } from "vitest";
import { clean, cleanDeep, cleanText } from "./clean";

describe("clean", () => {
  it("matches the Rust test case", () => {
    expect(clean("a\x1b]52;c;ZXZpbA==\x07b\x1b[2Jc\u009b31md\re\tf‮g")).toBe(
      "a\\u{1b}]52;c;ZXZpbA==\\u{7}b\\u{1b}[2Jc\\u{9b}31md e    f\\u{202e}g",
    );
  });
  it("keeps normal text, emoji and CJK", () => {
    expect(clean("héllo 🦆 世界")).toBe("héllo 🦆 世界");
  });
  it("shows every bidi control", () => {
    for (const c of [0x200e, 0x200f, 0x202a, 0x202e, 0x2066, 0x2069]) {
      expect(clean(String.fromCodePoint(c))).toBe(`\\u{${c.toString(16)}}`);
    }
  });
  it("cleanText keeps newlines and drops carriage returns", () => {
    expect(cleanText("a\r\nb\t\x07")).toBe("a\nb    \\u{7}");
  });
  it("cleanDeep reaches values and keys", () => {
    expect(cleanDeep({ "k‮": ["v\x1b", 1, null, { x: "⁦" }] })).toEqual({
      "k\\u{202e}": ["v\\u{1b}", 1, null, { x: "\\u{2066}" }],
    });
  });
});
