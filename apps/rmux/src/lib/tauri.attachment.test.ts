import { beforeEach, describe, expect, it, vi } from "vitest";
import type { OpenAttachmentRequest, OpenAttachmentResponse } from "./types";
import { openAttachment } from "./tauri";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: ipc.invoke,
  Channel: class { onmessage = (_message: unknown) => {}; },
}));

const request: OpenAttachmentRequest = {
  target: { kind: "ssh", destination: "offline" },
  session: "shell",
  resume_from: null,
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
  request_input_lease: true,
  request_layout_lease: false,
};

beforeEach(() => { vi.resetAllMocks(); });

describe("attachment IPC cancellation", () => {
  it.each([true, false])("cancels before or after the opening notification (before=%s)", async (before) => {
    let reject_open!: (error: unknown) => void;
    let notify_opening!: (id: string) => void;
    ipc.invoke.mockImplementation((command, args) => {
      if (command === "open_attachment") {
        notify_opening = args.on_opening.onmessage;
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
    notify_opening("registered-id");
    if (!before) controller.abort();
    await cancelled;
    expect(ipc.invoke).toHaveBeenCalledTimes(2);
    expect(ipc.invoke).toHaveBeenLastCalledWith("cancel_attachment_open", {
      request: { attachment_id: "registered-id" },
    });
  });

  it("does not start an already-cancelled request", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(openAttachment(request, vi.fn(), controller.signal)).rejects.toThrow();
    expect(ipc.invoke).not.toHaveBeenCalled();
  });

  it("removes cancellation after the open settles and preserves the response", async () => {
    const response = { attachment_id: "active", session: { session_id: "shell" } } as OpenAttachmentResponse;
    ipc.invoke.mockImplementation(async (_command, args) => {
      args.on_opening.onmessage("active");
      return response;
    });
    const controller = new AbortController();
    const result = await openAttachment(request, vi.fn(), controller.signal);
    controller.abort();
    expect(result.attached.session.target).toEqual(request.target);
    expect(ipc.invoke).toHaveBeenCalledOnce();
  });
});
