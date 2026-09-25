import { describe, expect, it } from "vitest";
import { InputPump } from "./InputPump";

describe("InputPump", () => {
  it("preserves input order with one send in flight", async () => {
    const sent: number[] = [];
    let releaseFirst!: () => void;
    const firstPending = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const pump = new InputPump(async (data) => {
      if (sent.length === 0) {
        sent.push(data[0]);
        await firstPending;
      } else {
        sent.push(data[0]);
      }
    }, () => undefined);

    expect(pump.push(new Uint8Array([1]))).toBe(true);
    expect(pump.push(new Uint8Array([2]))).toBe(true);
    await Promise.resolve();
    expect(sent).toEqual([1]);
    releaseFirst();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(sent).toEqual([1, 2]);
  });

  it("rejects input once its bounded queue is full", () => {
    const pump = new InputPump(
      () => new Promise(() => undefined),
      () => undefined,
      2,
    );

    expect(pump.push(new Uint8Array([1, 2]))).toBe(true);
    expect(pump.push(new Uint8Array([3]))).toBe(false);
  });

  it("clears queued input without corrupting in-flight byte accounting", async () => {
    const sent: number[] = [];
    let releaseFirst!: () => void;
    const firstPending = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const pump = new InputPump(async (data) => {
      sent.push(data[0]);
      if (sent.length === 1) {
        await firstPending;
      }
    }, () => undefined, 4);

    expect(pump.push(new Uint8Array([1, 1]))).toBe(true);
    expect(pump.push(new Uint8Array([2, 2]))).toBe(true);
    await Promise.resolve();
    pump.clear();

    expect(pump.push(new Uint8Array([3, 3]))).toBe(true);
    expect(pump.push(new Uint8Array([4]))).toBe(false);
    releaseFirst();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(sent).toEqual([1, 3]);
    expect(pump.push(new Uint8Array([4, 4, 4, 4]))).toBe(true);
  });
});
