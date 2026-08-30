import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import type { TerminalSize } from "../../lib/types";
import {
  TerminalPresenter,
  type TerminalAdapter,
} from "./TerminalPresenter";

function terminalSize(columns: number, rows: number): TerminalSize {
  return {
    columns,
    rows,
    pixel_width: null,
    pixel_height: null,
  };
}

function visibleLine(terminal: Terminal, row: number): string {
  return terminal.buffer.active.getLine(row)?.translateToString(true) ?? "";
}

function headlessFactory(instances: Terminal[]) {
  return (size: TerminalSize): TerminalAdapter => {
    const terminal = new Terminal({
      cols: size.columns,
      rows: size.rows,
      allowProposedApi: true,
    });
    instances.push(terminal);
    return {
      write: (data, callback) => terminal.write(data, callback),
      resize: (columns, rows) => terminal.resize(columns, rows),
      dispose: () => terminal.dispose(),
    };
  };
}

describe("TerminalPresenter", () => {
  it("recreates a clean renderer for a checkpoint", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(12, 3));
    await presenter.write(new TextEncoder().encode("dirty state"));

    await presenter.restoreCheckpoint(
      terminalSize(8, 2),
      new TextEncoder().encode("restored"),
      new Uint8Array(),
    );

    expect(instances).toHaveLength(2);
    expect(instances[1].cols).toBe(8);
    expect(instances[1].rows).toBe(2);
    expect(visibleLine(instances[1], 0)).toBe("restored");
  });

  it("keeps one byte decoder across checkpoint prefix and later output", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(10, 2));

    await presenter.restoreCheckpoint(
      terminalSize(10, 2),
      new TextEncoder().encode("amount: "),
      new Uint8Array([0xe2, 0x82]),
    );
    await presenter.write(new Uint8Array([0xac]));

    expect(visibleLine(instances[1], 0)).toBe("amount: €");
  });

  it("serializes output and authoritative geometry changes", async () => {
    const calls: string[] = [];
    const presenter = new TerminalPresenter(
      () => ({
        write: (_data, callback) => {
          calls.push("write:start");
          queueMicrotask(() => {
            calls.push("write:end");
            callback();
          });
        },
        resize: (columns, rows) => calls.push(`resize:${columns}x${rows}`),
        dispose: () => undefined,
      }),
      terminalSize(80, 24),
    );

    const write = presenter.write(new Uint8Array([1]));
    const resize = presenter.resize(terminalSize(120, 32));
    await Promise.all([write, resize]);

    expect(calls).toEqual(["write:start", "write:end", "resize:120x32"]);
  });
});
