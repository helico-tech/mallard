import { describe, expect, it } from "vitest";
import { accept, quoteValue, tokenAt } from "./suggest";

describe("tokenAt", () => {
  it("finds the word under the caret", () => {
    expect(tokenAt("", 0)).toEqual({ start: 0, end: 0, key: null, text: "" });
    expect(tokenAt("errors ser", 10)).toEqual({ start: 7, end: 10, key: null, text: "ser" });
    expect(tokenAt("errors ", 7)).toEqual({ start: 7, end: 7, key: null, text: "" });
  });
  it("splits key and value, up to the caret", () => {
    expect(tokenAt("service=che", 11)).toEqual({ start: 0, end: 11, key: "service", text: "che" });
    expect(tokenAt("service=checkout errors", 11)).toEqual({ start: 0, end: 16, key: "service", text: "che" });
  });
  it("quotes keep their spaces", () => {
    expect(tokenAt('op="GET /x', 10)).toEqual({ start: 0, end: 10, key: "op", text: "GET /x" });
    expect(tokenAt('op="GET /x" errors', 18).start).toBe(12);
  });
});

describe("accept", () => {
  it("a key stays open for its value", () => {
    expect(accept("errors ser", tokenAt("errors ser", 10), "service=")).toEqual({ text: "errors service=", caret: 15, more: true });
  });
  it("a value ends the token", () => {
    expect(accept("service=c", tokenAt("service=c", 9), "service=checkout-api")).toEqual({ text: "service=checkout-api ", caret: 21, more: false });
  });
  it("what follows the caret stays", () => {
    expect(accept("op=x errors", tokenAt("op=x errors", 4), "op=y").text).toBe("op=y errors");
  });
  it("values with spaces go in quotes", () => {
    expect(quoteValue("GET /x")).toBe('"GET /x"');
    expect(quoteValue("plain")).toBe("plain");
  });
});
