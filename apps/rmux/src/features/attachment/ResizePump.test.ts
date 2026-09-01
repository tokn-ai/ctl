import { afterEach, describe, expect, it, vi } from "vitest";
import type { TerminalSize } from "../../lib/types";
import { ResizePump, type PendingResize } from "./ResizePump";

function resize(columns: number, rows: number): PendingResize {
  const terminalSize: TerminalSize = {
    columns,
    rows,
    pixel_width: null,
    pixel_height: null,
  };
  return {
    attachment_id: "attachment",
    generation: 1,
    terminal_size: terminalSize,
  };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe("ResizePump", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("debounces a burst to its latest grid", async () => {
    vi.useFakeTimers();
    const sent: PendingResize[] = [];
    const pump = new ResizePump(async (value) => {
      sent.push(value);
    }, () => undefined);

    pump.schedule(resize(80, 24));
    pump.schedule(resize(100, 30));
    pump.schedule(resize(120, 36));
    await vi.advanceTimersByTimeAsync(79);
    expect(sent).toEqual([]);

    await vi.advanceTimersByTimeAsync(1);
    expect(sent.map((value) => value.terminal_size)).toEqual([
      expect.objectContaining({ columns: 120, rows: 36 }),
    ]);
  });

  it("keeps one send in flight and then sends only the latest grid", async () => {
    vi.useFakeTimers();
    const sent: PendingResize[] = [];
    let releaseFirst!: () => void;
    const firstPending = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const pump = new ResizePump(async (value) => {
      sent.push(value);
      if (sent.length === 1) {
        await firstPending;
      }
    }, () => undefined);

    pump.schedule(resize(80, 24));
    await vi.advanceTimersByTimeAsync(80);
    pump.schedule(resize(100, 30));
    pump.schedule(resize(120, 36));
    await vi.advanceTimersByTimeAsync(200);
    expect(sent).toHaveLength(1);

    releaseFirst();
    await settle();
    await vi.advanceTimersByTimeAsync(80);
    expect(sent.map((value) => value.terminal_size.columns)).toEqual([80, 120]);
  });

  it("cancels queued work when cleared", async () => {
    vi.useFakeTimers();
    const sent: PendingResize[] = [];
    const pump = new ResizePump(async (value) => {
      sent.push(value);
    }, () => undefined);

    pump.schedule(resize(120, 36));
    pump.clear();
    await vi.advanceTimersByTimeAsync(100);

    expect(sent).toEqual([]);
  });

  it("reports a failed resize", async () => {
    vi.useFakeTimers();
    const errors: Array<{ error: unknown; resize: PendingResize }> = [];
    const pump = new ResizePump(
      async () => {
        throw new Error("resize failed");
      },
      (error, value) => errors.push({ error, resize: value }),
    );

    pump.schedule(resize(120, 36));
    await vi.advanceTimersByTimeAsync(80);

    expect(errors).toHaveLength(1);
    expect(errors[0].error).toBeInstanceOf(Error);
    expect(errors[0].resize.terminal_size.columns).toBe(120);
  });

  it("preserves newer work when an in-flight resize fails", async () => {
    vi.useFakeTimers();
    const sent: number[] = [];
    let rejectFirst!: (error: Error) => void;
    const firstPending = new Promise<void>((_resolve, reject) => {
      rejectFirst = reject;
    });
    const pump = new ResizePump(async (value) => {
      sent.push(value.terminal_size.columns);
      if (sent.length === 1) {
        await firstPending;
      }
    }, () => undefined);

    pump.schedule(resize(80, 24));
    await vi.advanceTimersByTimeAsync(80);
    pump.schedule(resize(120, 36));
    rejectFirst(new Error("stale resize failed"));
    await settle();
    await vi.advanceTimersByTimeAsync(80);

    expect(sent).toEqual([80, 120]);
  });
});
