import { Terminal } from "@xterm/headless";
import { describe, expect, it, vi } from "vitest";
import type { TerminalSize } from "../../lib/types";
import {
  TerminalPresenter,
  type TerminalAdapter,
  type TerminalAdapterOptions,
  type HistoryPresentation,
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
  return (size: TerminalSize, options?: TerminalAdapterOptions): TerminalAdapter => {
    const terminal = new Terminal({
      cols: size.columns,
      rows: size.rows,
      allowProposedApi: true,
      scrollback: options?.scrollback_limit ?? 10_000,
    });
    instances.push(terminal);
    return {
      activate: (offset = 0) => terminal.scrollToLine(Math.max(0, terminal.buffer.active.baseY - offset)),
      viewportOffset: () => terminal.buffer.active.baseY - terminal.buffer.active.viewportY,
      copyLines: () => Array.from({ length: terminal.buffer.active.length }, (_, index) => visibleLine(terminal, index)),
      copyPrimaryRows: () => Array.from({ length: terminal.rows }, (_, index) => {
        const buffer = terminal.buffer.normal;
        const wrapped = buffer.getLine(buffer.baseY + index + 1)?.isWrapped ?? false;
        return { text: buffer.getLine(buffer.baseY + index)?.translateToString(!wrapped) ?? "", wrapped };
      }),
      write: (data, callback) => terminal.write(data, callback),
      resize: (columns, rows) => terminal.resize(columns, rows),
      dispose: () => terminal.dispose(),
    };
  };
}

function historySnapshot(payload: string, overrides: Partial<HistoryPresentation> = {}): HistoryPresentation {
  return {
    terminal_size: terminalSize(20, 2),
    rows: [{ text: "older history", wrapped: false }],
    payload: new TextEncoder().encode(payload),
    input_prefix: new Uint8Array(),
    sequence: "0",
    snapshot_id: "snapshot",
    scrollback_limit: "10000",
    ...overrides,
  };
}

describe("TerminalPresenter", () => {
  it("archives buffered output after pending terminal writes finish", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(20, 2));
    const pending = presenter.write(new TextEncoder().encode("old line\r\nlast output"));
    expect(await presenter.copyLines()).toEqual(["old line", "last output"]);
    await pending;
    presenter.dispose();
  });

  it("resets the live renderer in place for a checkpoint", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(12, 3));
    await presenter.write(new TextEncoder().encode("dirty state"));

    await presenter.restoreCheckpoint(
      terminalSize(8, 2),
      [],
      new TextEncoder().encode("restored"),
      new Uint8Array(),
    );

    expect(instances).toHaveLength(1);
    expect(instances[0].cols).toBe(8);
    expect(instances[0].rows).toBe(2);
    expect(visibleLine(instances[0], 0)).toBe("restored");
  });

  it("keeps one byte decoder across checkpoint prefix and later output", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(10, 2));

    await presenter.restoreCheckpoint(
      terminalSize(10, 2),
      [],
      new TextEncoder().encode("amount: "),
      new Uint8Array([0xe2, 0x82]),
    );
    await presenter.write(new Uint8Array([0xac]));

    expect(visibleLine(instances[0], 0)).toBe("amount: €");
  });

  it.each([
    ["UTF-8", new Uint8Array([0xe2, 0x82])],
    ["CSI", new TextEncoder().encode("\u001b[?1049")],
    ["OSC", new TextEncoder().encode("\u001b]0;unfinished")],
    ["OSC escape", new TextEncoder().encode("\u001b]0;unfinished\u001b")],
    ["DCS", new TextEncoder().encode("\u001bP1;2|unfinished")],
    ["DCS escape", new TextEncoder().encode("\u001bP1;2|unfinished\u001b")],
  ])("cancels an old partial %s before restoring a new decoder prefix", async (_kind, pending) => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(20, 2));
    await presenter.write(new TextEncoder().encode("old screen"));
    await presenter.write(pending);
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("amount: "), new Uint8Array([0xe2, 0x82]), "2", "new");
    await presenter.write(new Uint8Array([0xac]), "3", "2");
    expect(instances).toHaveLength(1);
    expect(await presenter.copyLines()).toEqual(["amount: €", ""]);
    expect(instances[0].buffer.active.type).toBe("normal");
    presenter.dispose();
  });

  it("resets old terminal modes and restores primary and alternate checkpoints in place", async () => {
    const instances: Terminal[] = [];
    const size = terminalSize(20, 2);
    const presenter = new TerminalPresenter(headlessFactory(instances), size);
    await presenter.write(new TextEncoder().encode("old\r\nhistory\r\nprimary\u001b[?1049halternate\u001b[?1h\u001b=\u001b[?2004h\u001b[?1000h\u001b[?1004h\u001b[4h\u001b[?6h\u001b[?7l"));
    await presenter.restoreCheckpoint(size, ["confirmed"], new TextEncoder().encode("new primary"), new Uint8Array());
    const terminal = instances[0];
    expect(terminal.buffer.active.type).toBe("normal");
    expect(terminal.modes).toMatchObject({ applicationCursorKeysMode: false, applicationKeypadMode: false,
      bracketedPasteMode: false, insertMode: false, mouseTrackingMode: "none", originMode: false,
      sendFocusMode: false, wraparoundMode: true });
    expect(await presenter.copyLines()).toEqual(["confirmed", "new primary", ""]);
    await presenter.restoreCheckpoint(size, [], new TextEncoder().encode("primary\u001b[?1049h\u001b[Halternate"), new Uint8Array());
    expect(terminal.buffer.active.type).toBe("alternate");
    expect(visibleLine(terminal, 0)).toBe("alternate");
    expect(terminal.buffer.normal.getLine(0)?.translateToString(true)).toBe("primary");
    expect(instances).toHaveLength(1);
    presenter.dispose();
  });

  it("preserves distance from live output across a replacing checkpoint", async () => {
    const instances: Terminal[] = [];
    const size = terminalSize(20, 2);
    const presenter = new TerminalPresenter(headlessFactory(instances), size);
    const history = Array.from({ length: 20 }, (_, index) => `history ${index}`);
    await presenter.restoreCheckpoint(size, history, new TextEncoder().encode("live"), new Uint8Array(), "0", "first");
    const terminal = instances[0];
    terminal.scrollToLine(3);
    const distance = terminal.buffer.active.baseY - terminal.buffer.active.viewportY;
    await presenter.restoreCheckpoint(terminalSize(18, 2), history, new TextEncoder().encode("new live"), new Uint8Array(), "8", "next");
    expect(terminal.buffer.active.baseY - terminal.buffer.active.viewportY).toBe(distance);
    expect(visibleLine(terminal, terminal.buffer.active.viewportY)).toBe("history 3");
    expect(instances).toHaveLength(1);
    presenter.dispose();
  });

  it("applies the reset, history, screen and decoder prefix in one ordered write", async () => {
    const writes: Uint8Array[] = [];
    const resize = vi.fn();
    const dispose = vi.fn();
    const presenter = new TerminalPresenter(() => ({
      write: (data, callback) => { writes.push(data); callback(); }, resize, dispose,
    }), terminalSize(20, 2));
    await presenter.restoreCheckpoint(terminalSize(18, 2), ["history"], new TextEncoder().encode("screen"), new Uint8Array([0xe2, 0x82]));
    expect(resize).toHaveBeenCalledWith(18, 2);
    expect(writes).toHaveLength(1);
    expect(new TextDecoder().decode(writes[0].subarray(0, writes[0].length - 2))).toBe("\u0018\u001bchistory\r\n\r\n\u001b[0m\u001b[Hscreen");
    expect([...writes[0].slice(-2)]).toEqual([0xe2, 0x82]);
    expect(dispose).not.toHaveBeenCalled();
    presenter.dispose();
  });

  it("restores normalized history above the live checkpoint", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(8, 2));

    await presenter.restoreCheckpoint(
      terminalSize(8, 2),
      ["old-one", "old-two"],
      new TextEncoder().encode("\u001b[2J\u001b[Hlive"),
      new Uint8Array(),
    );

    const buffer = instances[0].buffer.normal;
    expect(buffer.baseY).toBe(2);
    expect(buffer.getLine(0)?.translateToString(true)).toBe("old-one");
    expect(buffer.getLine(1)?.translateToString(true)).toBe("old-two");
    expect(buffer.getLine(buffer.baseY)?.translateToString(true)).toBe("live");
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

  it("lets an in-flight write finish on disposal and skips queued recreation", async () => {
    const calls: string[] = [];
    let finish!: () => void;
    const presenter = new TerminalPresenter(() => {
      calls.push("create");
      return {
        write: (_data, callback) => {
          calls.push("write");
          finish = callback;
        },
        resize: () => undefined,
        dispose: () => { calls.push("dispose"); },
      };
    }, terminalSize(80, 24));
    const pending = presenter.write(new Uint8Array([1]));
    await Promise.resolve();
    const recreate = presenter.recreate(terminalSize(100, 30));
    presenter.dispose();
    presenter.dispose();
    expect(calls).toEqual(["create", "write"]);
    finish();
    await Promise.all([pending, recreate]);
    expect(calls).toEqual(["create", "write", "dispose"]);
  });

  it("adds older history only after replay catches up to newer live output", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(20, 2));
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("live"), new Uint8Array(), "0", "snapshot");
    const live = instances[0];
    const syncing = presenter.syncHistory(historySnapshot("live"));
    await presenter.write(new TextEncoder().encode(" newer"), "6", "0");
    expect(visibleLine(live, 0)).toBe("live newer");
    expect(await syncing).toBe(true);
    const restored = instances[instances.length - 1];
    expect(restored.buffer.normal.getLine(0)?.translateToString(true)).toBe("older history");
    expect(restored.buffer.normal.getLine(restored.buffer.normal.baseY)?.translateToString(true)).toBe("live newer");
    presenter.dispose();
  });

  it("preserves the user's distance from live output when publishing older history", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(20, 2));
    const recent = Array.from({ length: 20 }, (_, index) => `history ${80 + index}`);
    await presenter.restoreCheckpoint(terminalSize(20, 2), recent, new TextEncoder().encode("live"), new Uint8Array(), "0", "snapshot");
    instances[0].scrollToLine(3);
    expect(instances[0].buffer.active.viewportY).toBe(3);
    const rows = Array.from({ length: 100 }, (_, index) => ({ text: `history ${index}`, wrapped: false }));
    expect(await presenter.syncHistory(historySnapshot("live", { rows }))).toBe(true);
    const restored = instances[instances.length - 1];
    expect(restored.buffer.active.viewportY).toBe(83);
    expect(visibleLine(restored, restored.buffer.active.viewportY)).toBe("history 83");
    presenter.dispose();
  });

  it("returns a live checkpoint before a large legacy history stage is ready", async () => {
    const instances: Terminal[] = [];
    const factory = headlessFactory(instances);
    let finish_stage!: () => void;
    const presenter = new TerminalPresenter((size, options) => {
      const adapter = factory(size, options);
      return options?.background ? { ...adapter, write: (data, callback) => adapter.write(data, () => { finish_stage = callback; }) } : adapter;
    }, terminalSize(20, 2));
    await presenter.restoreCheckpoint(terminalSize(20, 2), Array.from({ length: 5000 }, (_, index) => `history ${index}`),
      new TextEncoder().encode("live"), new Uint8Array(), "0");
    const lines = await presenter.copyLines();
    expect(lines).toHaveLength(66);
    expect(lines[64]).toBe("live");
    await vi.waitFor(() => expect(finish_stage).toBeTypeOf("function"));
    presenter.dispose();
    finish_stage();
  });

  it("preserves the checkpoint decoder prefix when newer bytes are replayed into history", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(20, 2));
    const prefix = new Uint8Array([0xe2, 0x82]);
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("amount: "), prefix, "2", "snapshot");
    await presenter.write(new Uint8Array([0xac]), "3", "2");
    expect(await presenter.syncHistory(historySnapshot("amount: ", { sequence: "2", input_prefix: prefix }))).toBe(true);
    const restored = instances[instances.length - 1];
    expect(restored.buffer.normal.getLine(restored.buffer.normal.baseY)?.translateToString(true)).toBe("amount: €");
    presenter.dispose();
  });

  it("preserves a partial wrapped prefix across the history and primary grid", async () => {
    const instances: Terminal[] = [];
    const size = terminalSize(8, 2);
    const presenter = new TerminalPresenter(headlessFactory(instances), size);
    await presenter.restoreCheckpoint(size, [], new TextEncoder().encode("ijkl"), new Uint8Array(), "0", "snapshot");
    expect(await presenter.syncHistory(historySnapshot("ijkl", { terminal_size: size, rows: [{ text: "abcdefgh", wrapped: true }] }))).toBe(true);
    const restored = instances[instances.length - 1];
    const buffer = restored.buffer.normal;
    expect(buffer.baseY).toBe(1);
    expect(buffer.getLine(0)?.translateToString(true)).toBe("abcdefgh");
    expect(buffer.getLine(1)?.translateToString(true)).toBe("ijkl");
    expect(buffer.getLine(1)?.isWrapped).toBe(true);
    // xterm deliberately leaves the cursor's logical line out of reflow.
    await presenter.write(new TextEncoder().encode("\u001b[2;1H"), "6", "0");
    await presenter.resize(terminalSize(16, 2));
    expect(restored.buffer.normal.getLine(0)?.translateToString(true)).toBe("abcdefghijkl");
    presenter.dispose();
  });

  it("seeds primary history while preserving the current alternate screen and updates", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(20, 2));
    const payload = "primary\u001b[?1049h\u001b[Halternate";
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode(payload), new Uint8Array(), "0", "snapshot");
    await presenter.write(new TextEncoder().encode("!"), "1", "0");
    expect(await presenter.syncHistory(historySnapshot(payload))).toBe(true);
    const restored = instances[instances.length - 1];
    expect(restored.buffer.active.type).toBe("alternate");
    expect(visibleLine(restored, 0)).toBe("alternate!");
    expect(restored.buffer.normal.getLine(0)?.translateToString(true)).toBe("older history");
    expect(restored.buffer.normal.getLine(restored.buffer.normal.baseY)?.translateToString(true)).toBe("primary");
    presenter.dispose();
  });

  it("retains history below a wrapped primary row whose cells were erased", async () => {
    const instances: Terminal[] = [];
    const size = terminalSize(8, 2);
    const payload = "\u001b[Habcdefghijk\u001b[1;1H\u001b[2K\u001b[2;4H";
    const presenter = new TerminalPresenter(headlessFactory(instances), size);
    await presenter.restoreCheckpoint(size, [], new TextEncoder().encode(payload), new Uint8Array(), "0", "snapshot");
    expect(await presenter.syncHistory(historySnapshot(payload, { terminal_size: size, rows: [{ text: "history", wrapped: false }] }))).toBe(true);
    const restored = instances[instances.length - 1];
    const buffer = restored.buffer.normal;
    expect(buffer.baseY).toBe(1);
    expect(buffer.getLine(0)?.translateToString(true)).toBe("history");
    expect(buffer.getLine(1)?.translateToString(true)).toBe("");
    expect(buffer.getLine(2)?.translateToString(true)).toBe("ijk");
    expect(buffer.getLine(2)?.isWrapped).toBe(true);
    presenter.dispose();
  });

  it("rejects stale snapshot IDs and control characters without changing the live screen", async () => {
    const instances: Terminal[] = [];
    const presenter = new TerminalPresenter(headlessFactory(instances), terminalSize(20, 2));
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("live"), new Uint8Array(), "0", "snapshot");
    expect(await presenter.syncHistory(historySnapshot("old", { snapshot_id: "stale" }))).toBe(false);
    expect(instances).toHaveLength(1);
    expect(await presenter.syncHistory(historySnapshot("old", { rows: [{ text: "\u001b[2Jinjected", wrapped: false }] }))).toBe(false);
    expect(await presenter.copyLines()).toEqual(["live", ""]);
    presenter.dispose();
  });

  it("keeps the live adapter usable if a staged adapter cannot be activated", async () => {
    const instances: Terminal[] = [];
    const factory = headlessFactory(instances);
    const presenter = new TerminalPresenter((size, options) => {
      const adapter = factory(size, options);
      return options?.background ? { ...adapter, activate: () => { throw new Error("Mount unavailable"); } } : adapter;
    }, terminalSize(20, 2));
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("live"), new Uint8Array(), "0", "snapshot");
    expect(await presenter.syncHistory(historySnapshot("live"))).toBe(false);
    await presenter.write(new TextEncoder().encode("!"), "1", "0");
    expect(await presenter.copyLines()).toEqual(["live!", ""]);
    presenter.dispose();
  });

  it.each(["checkpoint", "resize", "detach", "dispose"])("discards a pending history stage on %s", async (transition) => {
    const instances: Terminal[] = [];
    const factory = headlessFactory(instances);
    let finish_stage!: () => void;
    const activate = vi.fn();
    const presenter = new TerminalPresenter((size, options) => {
      const adapter = factory(size, options);
      return options?.background ? {
        ...adapter,
        activate,
        write: (data, callback) => adapter.write(data, () => { finish_stage = callback; }),
      } : adapter;
    }, terminalSize(20, 2));
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("live"), new Uint8Array(), "0", "snapshot");
    const syncing = presenter.syncHistory(historySnapshot("live"));
    await vi.waitFor(() => expect(finish_stage).toBeTypeOf("function"));
    if (transition === "checkpoint") await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("new screen"), new Uint8Array(), "5", "next");
    else if (transition === "resize") await presenter.resize(terminalSize(30, 2));
    else if (transition === "detach") presenter.cancelHistory();
    else presenter.dispose();
    finish_stage();
    expect(await syncing).toBe(false);
    expect(activate).not.toHaveBeenCalled();
    if (transition !== "dispose") expect((await presenter.copyLines())[0]).toBe(transition === "checkpoint" ? "new screen" : "live");
    presenter.dispose();
  });

  it("drops history outside the bounded replay window instead of restoring an older screen", async () => {
    const factory = vi.fn((): TerminalAdapter => ({ write: (_data, callback) => callback(), resize: () => undefined, dispose: () => undefined }));
    const presenter = new TerminalPresenter(factory, terminalSize(20, 2));
    await presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("live"), new Uint8Array(), "0", "snapshot");
    const output = new Uint8Array(1024 * 1024 + 1);
    await presenter.write(output, String(output.length), "0");
    expect(await presenter.syncHistory(historySnapshot("old"))).toBe(false);
    expect(factory).toHaveBeenCalledTimes(1);
    presenter.dispose();
  });

  it("does not reenable a snapshot cancelled while its live checkpoint is still writing", async () => {
    const instances: Terminal[] = [];
    const factory = headlessFactory(instances);
    let finish!: () => void;
    const create = vi.fn((size: TerminalSize, options?: TerminalAdapterOptions) => {
      const adapter = factory(size, options);
      return { ...adapter, write: (data: Uint8Array, callback: () => void) => adapter.write(data, () => {
        if (new TextDecoder().decode(data).endsWith("live")) finish = callback;
        else callback();
      }) };
    });
    const presenter = new TerminalPresenter(create, terminalSize(20, 2));
    const restoring = presenter.restoreCheckpoint(terminalSize(20, 2), [], new TextEncoder().encode("live"), new Uint8Array(), "0", "snapshot");
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    presenter.cancelHistory();
    finish();
    await restoring;
    expect(await presenter.syncHistory(historySnapshot("live"))).toBe(false);
    expect(create).toHaveBeenCalledTimes(1);
    presenter.dispose();
  });
});
