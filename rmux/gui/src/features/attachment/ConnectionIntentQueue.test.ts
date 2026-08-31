import { describe, expect, it } from "vitest";
import { ConnectionIntentQueue } from "./ConnectionIntentQueue";

describe("ConnectionIntentQueue", () => {
  it("keeps only the newest deferred connection intent", async () => {
    const queue = new ConnectionIntentQueue<string>();
    queue.begin("first");
    let firstSettled = false;
    const first = queue.defer("first").then(() => {
      firstSettled = true;
    });
    queue.begin("second");
    const second = queue.defer("second");

    await first;
    expect(firstSettled).toBe(true);
    const deferred = queue.take();
    expect(deferred?.request).toBe("second");
    deferred?.settle();
    await expect(second).resolves.toBeUndefined();
  });

  it("settles a deferred request when it is cancelled", async () => {
    const queue = new ConnectionIntentQueue<string>();
    queue.begin("session");
    const pending = queue.defer("session");

    queue.cancel();

    await expect(pending).resolves.toBeUndefined();
    expect(queue.take()).toBeNull();
  });

  it("does not cancel a newer session when an older tab is closed", async () => {
    const queue = new ConnectionIntentQueue<{ session_id: string }>();
    const second = { session_id: "second" };
    queue.begin(second);
    const secondPending = queue.defer(second);
    const third = { session_id: "third" };
    queue.begin(third);
    const thirdPending = queue.defer(third);

    await expect(secondPending).resolves.toBeUndefined();
    expect(queue.cancelIf(({ session_id }) => session_id === "second")).toBe(false);
    expect(queue.isCurrent(third)).toBe(true);
    const deferred = queue.take();
    expect(deferred?.request).toBe(third);
    deferred?.settle();

    await expect(thirdPending).resolves.toBeUndefined();
  });
});
