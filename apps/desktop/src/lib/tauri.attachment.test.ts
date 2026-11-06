// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Channel } from "@tauri-apps/api/core";
import type { AttachmentEvent, OpenAttachmentRequest, OpenAttachmentResponse } from "./types";
import { openAttachment, resizeAttachmentDivider, resizeAttachmentPane, requestAttachmentCheckpoint, watchTaskLogs } from "./tauri";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", async (load) => ({
  ...await load<typeof import("@tauri-apps/api/core")>(),
  invoke: ipc.invoke,
}));

const callbacks = new Map<number, (value: unknown) => void>();
function deliver<T>(channel: Channel<T>, index: number, message: T): void {
  callbacks.get(channel.id)?.({ index, message });
}

const request: OpenAttachmentRequest = {
  target: { kind: "ssh", destination: "offline" },
  session: "shell",
  resume_from: null,
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
  request_input_lease: true,
  request_layout_lease: false,
};
const attached = { attachment_id: "active", session: { session_id: "shell" } } as OpenAttachmentResponse;

beforeEach(() => {
  vi.resetAllMocks();
  callbacks.clear();
  let id = 0;
  vi.stubGlobal("__TAURI_INTERNALS__", {
    transformCallback: (callback: (value: unknown) => void) => { callbacks.set(++id, callback); return id; },
    unregisterCallback: (id: number) => callbacks.delete(id),
  });
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("attachment IPC cancellation", () => {
  it("keeps the verified environment while retaining saved alias metadata", async () => {
    const target = { kind: "ssh" as const, destination: "alias", host_id: "saved-host", host_name: "My host", method_id: "ssh-method" };
    const remote_info = { remote_id: "verified-environment", agent_version: "0.1.0" };
    ipc.invoke.mockResolvedValue({ attachment_id: "active", session: {
      session_id: "shell", target: { kind: "ssh", destination: "alias", remote_info },
    } } as OpenAttachmentResponse);
    const result = await openAttachment({ ...request, target }, vi.fn());
    expect(result.attached.session.target).toEqual({ ...target, remote_info });
    expect(ipc.invoke.mock.calls[0][1].request.cache_host_key).toBe("host:saved-host");
  });
  it("queues an exact divider target and lossless revision through the owning attachment", async () => {
    const request = { attachment_id: "owner", request_id: "operation", view_id: "view",
      expected_revision: "18446744073709551615", split_path: [0, 2], boundary: 1, position: 60 };
    await resizeAttachmentDivider(request);
    expect(ipc.invoke).toHaveBeenCalledExactlyOnceWith("resize_attachment_divider", { request });
  });
  it("queues a correlated pane resize through only the owning attachment", async () => {
    const request = { attachment_id: "owner", request_id: "operation", terminal_id: "secondary", direction: "left" as const, amount: 5 };
    await resizeAttachmentPane(request);
    expect(ipc.invoke).toHaveBeenCalledExactlyOnceWith("resize_attachment_pane", { request });
  });
  it("requests a checkpoint for only the current attachment", async () => {
    ipc.invoke.mockResolvedValue(undefined);
    await requestAttachmentCheckpoint({ attachment_id: "current" });
    expect(ipc.invoke).toHaveBeenCalledExactlyOnceWith("request_attachment_checkpoint", {
      request: { attachment_id: "current" },
    });
  });

  it.each([true, false])("cancels before or after the opening notification (before=%s)", async (before) => {
    let reject_open!: (error: unknown) => void;
    let notify_opening!: Channel<string>;
    ipc.invoke.mockImplementation((command, args) => {
      if (command === "open_attachment") {
        notify_opening = args.on_opening;
        return new Promise((_resolve, reject) => { reject_open = reject; });
      }
      reject_open({ code: "attachment_cancelled" });
      return Promise.resolve();
    });
    const controller = new AbortController();
    const opening = openAttachment(request, vi.fn(), controller.signal);
    const cancelled = expect(opening).rejects.toMatchObject({ code: "attachment_cancelled" });
    if (before) controller.abort();
    expect(ipc.invoke).toHaveBeenCalledTimes(1);
    deliver(notify_opening, 0, "registered-id");
    if (!before) controller.abort();
    await cancelled;
    expect(ipc.invoke).toHaveBeenCalledTimes(2);
    expect(ipc.invoke).toHaveBeenLastCalledWith("cancel_attachment_open", {
      request: { attachment_id: "registered-id" },
    });
    expect(callbacks.size).toBe(0);
  });

  it("does not start an already-cancelled request", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(openAttachment(request, vi.fn(), controller.signal)).rejects.toThrow();
    expect(ipc.invoke).not.toHaveBeenCalled();
  });

  it("removes cancellation after the open settles and preserves the response", async () => {
    ipc.invoke.mockImplementation(async (_command, args) => {
      deliver(args.on_opening, 0, "active");
      return attached;
    });
    const controller = new AbortController();
    const result = await openAttachment(request, vi.fn(), controller.signal);
    controller.abort();
    expect(result.attached.session.target).toEqual(request.target);
    expect(ipc.invoke).toHaveBeenCalledOnce();
    expect(callbacks.size).toBe(0);
  });
});

describe("attachment delivery", () => {
  it("releases a channel that cannot deliver later events after an index is lost", async () => {
    vi.useFakeTimers();
    vi.spyOn(document, "hidden", "get").mockReturnValue(false);
    ipc.invoke.mockResolvedValue(attached);
    const on_event = vi.fn();
    const stalled = vi.fn();
    await openAttachment(request, on_event, undefined, stalled);
    const channel = ipc.invoke.mock.calls[0][1].on_event as Channel<AttachmentEvent>;
    const stats = { event_type: "attachment_statistics", attachment_id: "active" } as AttachmentEvent;
    deliver(channel, 0, stats);
    deliver(channel, 2, stats);
    callbacks.get(channel.id)?.({ index: 3, end: true });
    expect(on_event).toHaveBeenCalledOnce();
    expect(callbacks.has(channel.id)).toBe(true);

    await vi.advanceTimersByTimeAsync(35_000);
    expect(stalled).toHaveBeenCalledOnce();
    expect(callbacks.has(channel.id)).toBe(false);
  });

  it("waits for foreground silence and stops watching when an ordered exit arrives", async () => {
    vi.useFakeTimers();
    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    ipc.invoke.mockResolvedValue(attached);
    const on_event = vi.fn();
    const stalled = vi.fn();
    const { close } = await openAttachment(request, on_event, undefined, stalled);
    const channel = ipc.invoke.mock.calls[0][1].on_event as Channel<AttachmentEvent>;
    await vi.advanceTimersByTimeAsync(120_000);
    expect(stalled).not.toHaveBeenCalled();

    hidden.mockReturnValue(false);
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(25_000);
    const exit = { event_type: "attachment_exited", attachment_id: "active", reason: "detached" } as AttachmentEvent;
    deliver(channel, 0, exit);
    await vi.advanceTimersByTimeAsync(35_000);

    expect(on_event).toHaveBeenCalledExactlyOnceWith(exit);
    expect(stalled).not.toHaveBeenCalled();
    // The renderer processed the final event even though the native end
    // marker did not arrive. Cleanup can still release the registration.
    close();
    close();
    expect(callbacks.size).toBe(0);
  });

  it("releases a task log callback when its pane closes before the subscription opens", async () => {
    let finish!: (id: string) => void;
    ipc.invoke.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const controller = new AbortController();
    const watching = watchTaskLogs("task", null, vi.fn(), controller.signal);
    expect(callbacks.size).toBe(1);
    controller.abort();
    expect(callbacks.size).toBe(0);
    finish("subscription");
    await expect(watching).resolves.toBe("subscription");
  });
});
