import { describe, expect, it } from "vitest";
import { terminalHistoryBytes, type HistoryBuffer } from "./historyBytes";

function buffer(rows: { text: string; wrapped?: boolean }[], base_y = rows.length): HistoryBuffer {
  return {
    baseY: base_y,
    getLine: (row) => rows[row] ? { isWrapped: rows[row].wrapped ?? false, translateToString: () => rows[row].text } : undefined,
  };
}

describe("terminal history bytes", () => {
  it("counts UTF-8 and line endings, preserving spaces inside wrapped logical lines", () => {
    expect(terminalHistoryBytes(buffer([
      { text: "界  " }, { text: "é  ", wrapped: true }, { text: "   " },
    ]))).toBe(new TextEncoder().encode("界  é\n\n").length);
  });

  it("excludes the live screen and a wrapped prefix continuing into it", () => {
    expect(terminalHistoryBytes(buffer([
      { text: "complete" }, { text: "prefix" }, { text: "live", wrapped: true }, { text: "screen" },
    ], 2))).toBe(9);
  });

  it("matches Rust Unicode whitespace, retaining BOM while trimming NEL", () => {
    expect(terminalHistoryBytes(buffer([{ text: "a\u0085" }, { text: "b\ufeff" }]))).toBe(7);
  });

  it("measures the entire retained buffer beyond one MiB", () => {
    const line = "x".repeat(1024);
    expect(terminalHistoryBytes(buffer(Array.from({ length: 1500 }, () => ({ text: line }))))).toBe(1500 * 1025);
  });
});
