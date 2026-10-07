import { afterEach, describe, expect, it, vi } from "vitest";
import type { TerminalSize } from "../../lib/types";
import { ResizeCoordinator } from "./ResizeCoordinator";
import { ResizePump } from "./ResizePump";

function terminalSize(columns: number, rows: number): TerminalSize {
  return {
    columns,
    rows,
    pixel_width: null,
    pixel_height: null,
  };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

function createTestCoordinator(send: (terminalSize: TerminalSize) => Promise<void>) {
  const pump = new ResizePump(
    async (resize) => send(resize.terminal_size),
    () => undefined,
  );
  const coordinator = new ResizeCoordinator(
    (size) =>
      pump.schedule({
        attachment_id: "attachment",
        generation: 1,
        terminal_size: size,
      }),
    () => pump.clear(),
  );
  return coordinator;
}

describe("ResizeCoordinator", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("cancels a stale resize when the viewport returns to authoritative size", async () => {
    vi.useFakeTimers();
    const sent: TerminalSize[] = [];
    const coordinator = createTestCoordinator(async (size) => {
      sent.push(size);
    });
    const original = terminalSize(80, 24);

    coordinator.reset(original);
    coordinator.setEnabled(true);
    coordinator.setDesired(terminalSize(120, 36));
    coordinator.setDesired(original);
    await vi.advanceTimersByTimeAsync(100);

    expect(sent).toEqual([]);
  });

  it("restores the desired grid after a stale resize was already in flight", async () => {
    vi.useFakeTimers();
    const sent: TerminalSize[] = [];
    let releaseFirst!: () => void;
    const firstPending = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const coordinator = createTestCoordinator(async (size) => {
      sent.push(size);
      if (sent.length === 1) {
        await firstPending;
      }
    });
    const original = terminalSize(80, 24);
    const stale = terminalSize(120, 36);

    coordinator.reset(original);
    coordinator.setEnabled(true);
    coordinator.setDesired(stale);
    await vi.advanceTimersByTimeAsync(80);
    coordinator.setDesired(original);
    coordinator.setAuthoritative(stale);
    releaseFirst();
    await settle();
    await vi.advanceTimersByTimeAsync(80);

    expect(sent.map((size) => size.columns)).toEqual([120, 80]);
  });

  it("reconciles after checkpoint recovery changes authoritative geometry", async () => {
    vi.useFakeTimers();
    const sent: TerminalSize[] = [];
    const coordinator = createTestCoordinator(async (size) => {
      sent.push(size);
    });
    const desired = terminalSize(80, 24);

    coordinator.reset(desired);
    coordinator.setDesired(desired);
    coordinator.setEnabled(true);
    coordinator.setAuthoritative(terminalSize(120, 36));
    await vi.advanceTimersByTimeAsync(80);

    expect(sent).toEqual([desired]);
  });

  it("holds queued resizes until every suspension ends and resumes only the latest desired grid", async () => {
    vi.useFakeTimers();
    const sent: TerminalSize[] = [];
    const coordinator = createTestCoordinator(async (size) => { sent.push(size); });
    coordinator.reset(terminalSize(80, 24));
    coordinator.setEnabled(true);
    coordinator.setDesired(terminalSize(90, 24));
    await vi.advanceTimersByTimeAsync(40);
    const first = coordinator.suspend();
    const second = coordinator.suspend();
    coordinator.setDesired(terminalSize(100, 30));
    coordinator.setAuthoritative(terminalSize(79, 24));
    coordinator.setDesired(terminalSize(120, 36));
    await vi.advanceTimersByTimeAsync(100);
    expect(sent).toEqual([]);
    first(); first();
    await vi.advanceTimersByTimeAsync(100);
    expect(sent).toEqual([]);
    second(); second();
    await vi.advanceTimersByTimeAsync(100);
    expect(sent).toEqual([terminalSize(120, 36)]);
  });

  it.each(["disable", "stop"])("does not restart automatic sizing after %s during a suspension", async (change) => {
    vi.useFakeTimers();
    const sent: TerminalSize[] = [];
    const coordinator = createTestCoordinator(async (size) => { sent.push(size); });
    coordinator.reset(terminalSize(80, 24));
    coordinator.setEnabled(true);
    const release = coordinator.suspend();
    coordinator.setDesired(terminalSize(120, 36));
    if (change === "disable") coordinator.setEnabled(false);
    else coordinator.stop();
    release();
    await vi.advanceTimersByTimeAsync(100);
    expect(sent).toEqual([]);
  });

  it("retains active suspensions across coordinator reset", async () => {
    vi.useFakeTimers();
    const sent: TerminalSize[] = [];
    const coordinator = createTestCoordinator(async (size) => { sent.push(size); });
    const release = coordinator.suspend();
    coordinator.reset(terminalSize(80, 24));
    coordinator.setDesired(terminalSize(100, 30));
    coordinator.setEnabled(true);
    await vi.advanceTimersByTimeAsync(100);
    expect(sent).toEqual([]);
    release();
    await vi.advanceTimersByTimeAsync(100);
    expect(sent).toEqual([terminalSize(100, 30)]);
  });
});
