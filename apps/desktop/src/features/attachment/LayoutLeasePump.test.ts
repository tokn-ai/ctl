import { describe, expect, it } from "vitest";
import {
  LayoutLeasePump,
  shouldStopResizeAfterLeaseStatus,
  type LayoutLeaseCommand,
} from "./LayoutLeasePump";

function command(acquire: boolean): LayoutLeaseCommand {
  return {
    attachment_id: "attachment",
    generation: 1,
    acquire,
  };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe("LayoutLeasePump", () => {
  it("does not treat an older release response as denial of newer resize intent", () => {
    expect(shouldStopResizeAfterLeaseStatus(true, false, false)).toBe(false);
    expect(shouldStopResizeAfterLeaseStatus(true, false, true)).toBe(true);
  });

  it("serializes a rapid on-to-off transition", async () => {
    const sent: boolean[] = [];
    let releaseAcquire!: () => void;
    const acquirePending = new Promise<void>((resolve) => {
      releaseAcquire = resolve;
    });
    const pump = new LayoutLeasePump(async (value) => {
      sent.push(value.acquire);
      if (sent.length === 1) {
        await acquirePending;
      }
    }, () => undefined);

    pump.schedule(command(true));
    pump.schedule(command(false));
    await settle();
    expect(sent).toEqual([true]);
    expect(pump.hasScheduledIntent("attachment", 1, false)).toBe(true);

    releaseAcquire();
    await settle();
    expect(sent).toEqual([true, false]);
    expect(pump.takeExpectedResponse("attachment", 1)).toBe(true);
    expect(pump.takeExpectedResponse("attachment", 1)).toBe(false);
  });

  it("coalesces waiting transitions to the latest intent", async () => {
    const sent: boolean[] = [];
    let releaseFirst!: () => void;
    const firstPending = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const pump = new LayoutLeasePump(async (value) => {
      sent.push(value.acquire);
      if (sent.length === 1) {
        await firstPending;
      }
    }, () => undefined);

    pump.schedule(command(true));
    pump.schedule(command(false));
    pump.schedule(command(true));
    releaseFirst();
    await settle();

    expect(sent).toEqual([true, true]);
  });

  it("removes a failed command from expected responses", async () => {
    const errors: unknown[] = [];
    const pump = new LayoutLeasePump(
      async () => {
        throw new Error("closed");
      },
      (error) => errors.push(error),
    );

    pump.schedule(command(true));
    await settle();

    expect(errors).toHaveLength(1);
    expect(pump.takeExpectedResponse("attachment", 1)).toBeNull();
  });
});
