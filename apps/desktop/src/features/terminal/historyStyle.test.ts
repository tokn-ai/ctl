import { describe, expect, it } from "vitest";
import { Terminal } from "@xterm/headless";
import { captureHistoryRow, historyRowAnsi } from "./historyStyle";

describe("history style validation", () => {
  it("roundtrips xterm's simultaneous bold and faint attributes", async () => {
    const source = new Terminal({ cols: 4, rows: 1, allowProposedApi: true });
    const restored = new Terminal({ cols: 4, rows: 1, allowProposedApi: true });
    await new Promise<void>((resolve) => source.write("\u001b[1;2mtext", resolve));
    const row = captureHistoryRow(source.buffer.normal.getLine(0), false);
    expect(row.styles![0].style).toMatchObject({ bold: true, faint: true });
    await new Promise<void>((resolve) => restored.write(historyRowAnsi(row), resolve));
    expect(restored.buffer.normal.getLine(0)!.getCell(0)!.isBold()).toBeTruthy();
    expect(restored.buffer.normal.getLine(0)!.getCell(0)!.isDim()).toBeTruthy();
    source.dispose();
    restored.dispose();
  });

  it.each([
    { start: 0, length: 0 },
    { start: -1, length: 1 },
    { start: 0.5, length: 1 },
    { start: 0, length: 2 },
    { start: 0xffffffff, length: 1 },
  ])("rejects invalid Unicode character ranges %j", (range) => {
    expect(() => historyRowAnsi({ text: "界", wrapped: false, styles: [{ ...range, style: { bold: true } }] })).toThrow();
  });

  it("rejects controls, overlapping spans, and malformed colors", () => {
    expect(() => historyRowAnsi({ text: "\u001b[3J", wrapped: false })).toThrow();
    expect(() => historyRowAnsi({ text: "x", wrapped: false, styles: [
      { start: 0, length: 1, style: { bold: true } }, { start: 0, length: 1, style: { italic: true } },
    ] })).toThrow();
    expect(() => historyRowAnsi({ text: "x", wrapped: false, styles: [
      { start: 0, length: 1, style: { foreground: { kind: "indexed", index: 256 } } },
    ] })).toThrow();
  });
});
