import { describe, expect, it } from "vitest";
import { LatestTaskQueue } from "./LatestTaskQueue";

describe("LatestTaskQueue", () => {
  it("runs one task at a time and coalesces waiting work to the latest task", async () => {
    const queue = new LatestTaskQueue();
    const ran: string[] = [];
    let releaseFirst!: () => void;
    const firstPending = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const submit = (name: string, wait: Promise<void> = Promise.resolve()) =>
      queue.submit(async () => {
        ran.push(name);
        await wait;
      }, () => undefined);

    const first = submit("first", firstPending);
    await Promise.resolve();
    const skipped = submit("skipped");
    const latest = submit("latest");
    await skipped;

    expect(ran).toEqual(["first"]);
    releaseFirst();
    await Promise.all([first, latest]);
    expect(ran).toEqual(["first", "latest"]);
  });

  it("continues after a failed task", async () => {
    const queue = new LatestTaskQueue();
    const errors: unknown[] = [];
    const failed = queue.submit(
      async () => {
        throw new Error("failed");
      },
      (error) => errors.push(error),
    );
    const next = queue.submit(async () => undefined, (error) => errors.push(error));

    await Promise.all([failed, next]);
    expect(errors).toHaveLength(1);
    expect(errors[0]).toBeInstanceOf(Error);
  });
});
