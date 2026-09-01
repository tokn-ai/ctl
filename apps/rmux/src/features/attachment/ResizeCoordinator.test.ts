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

function harness(send: (terminalSize: TerminalSize) => Promise<void>) {
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
    const coordinator = harness(async (size) => {
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
    const coordinator = harness(async (size) => {
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
    const coordinator = harness(async (size) => {
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
});
