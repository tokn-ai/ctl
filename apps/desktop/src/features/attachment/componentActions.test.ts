import { afterEach, describe, expect, it, vi } from "vitest";
import type { ComponentSessionsReset, DividerResize, SessionSummary, SessionView } from "../../lib/types";
import { componentResetMatches, publishPaneResizeResult, publishSessionView, reconnectComponentAttachments, registerAttachmentControl, resetComponentAttachments, resizeSessionDivider, resizeSessionPane, setSessionViewZoom } from "./componentActions";

const stops: (() => void)[] = [];
afterEach(() => { for (const stop of stops.splice(0)) stop(); });
const session = (host_id?: string, remote_id?: string): SessionSummary => ({
  target: host_id ? { kind: "ssh", host_id, destination: "sample", remote_info: remote_id ? { remote_id, agent_version: "0.1.0" } : undefined } : { kind: "local" },
  session_id: "shared-session-id", name: "shell", status: "running", terminal_size: { rows: 24, columns: 80, pixel_width: null, pixel_height: null }, next_sequence: "0",
});
function register(id: string, current: SessionSummary) {
  const control = { resizeDivider: vi.fn(async (_divider: DividerResize, _request_id: string) => {}), attachmentId: () => id, session: () => current, reconnect: vi.fn(async (): Promise<string | null> => `${id}-replacement`), reset: vi.fn(), layoutOwned: vi.fn(() => false), setViewZoom: vi.fn(async (_terminal_id: string | null) => {}), resizePane: vi.fn(async (_terminal_id: string, _direction: string, _amount: number, _request_id: string) => {}) };
  stops.push(registerAttachmentControl(control));
  return control;
}
const baseline = { view_id: "view", revision: "1", zoomed_terminal_id: null };

describe("component attachment actions", () => {
  it("routes an exact divider through a secondary layout owner and waits for the matching operation", async () => {
    const root = register("root", session());
    const secondary = register("secondary", { ...session(), terminal_id: "secondary" });
    secondary.layoutOwned.mockReturnValue(true);
    const divider = { view_id: "view", expected_revision: "1", split_path: [0], boundary: 2, position: 60 };
    const view: SessionView = { ...baseline, session_id: session().session_id, session_name: "shell", canvas_size: session().terminal_size,
      layout: { kind: "terminal", terminal_id: "secondary" }, panes: [], terminals: [] };
    let completed = false;
    const pending = resizeSessionDivider(session(), divider).then((view) => { completed = true; return view; });
    expect(root.resizeDivider).not.toHaveBeenCalled();
    expect(secondary.resizeDivider).toHaveBeenCalledExactlyOnceWith(divider, expect.any(String));
    const request_id = secondary.resizeDivider.mock.calls[0][1];
    publishPaneResizeResult({ session: session(), attachment_id: "root", request_id, view, error: null });
    publishPaneResizeResult({ session: session(), attachment_id: "secondary", request_id: "other-operation", view, error: null });
    await Promise.resolve(); expect(completed).toBe(false);
    publishPaneResizeResult({ session: session(), attachment_id: "secondary", request_id, view, error: null });
    expect(await pending).toEqual(view);
  });
  it("resizes through the actual owner and confirms only its exact operation, including no-ops", async () => {
    const root = register("root", session());
    const focused = register("focused", { ...session(), terminal_id: "secondary" });
    root.layoutOwned.mockReturnValue(true);
    const view: SessionView = {
      ...baseline, session_id: session().session_id, session_name: "shell", canvas_size: session().terminal_size,
      layout: { kind: "terminal", terminal_id: "secondary" }, panes: [], terminals: [],
    };
    let confirmed = false;
    const resizing = resizeSessionPane(session(), "secondary", "left", 5).then((next) => { confirmed = true; return next; });
    const request_id = root.resizePane.mock.calls[0][3];
    expect(root.resizePane).toHaveBeenCalledWith("secondary", "left", 5, expect.any(String));
    expect(focused.resizePane).not.toHaveBeenCalled();
    publishSessionView({ session: session(), attachment_id: "root", view: { ...view, revision: "9" } });
    publishPaneResizeResult({ session: session(), attachment_id: "root", request_id: "earlier-operation", view, error: null });
    publishPaneResizeResult({ session: session(), attachment_id: "focused", request_id, view, error: null });
    await Promise.resolve();
    expect(confirmed).toBe(false);
    // Equal revisions are valid only on the matching no-op acknowledgement.
    publishPaneResizeResult({ session: session(), attachment_id: "root", request_id, view, error: null });
    expect(await resizing).toEqual(view);
  });

  it("reports missing ownership, capability rejection and exact resize failures", async () => {
    const root = register("root", session());
    await expect(resizeSessionPane(session(), "secondary", "up", 1)).rejects.toThrow("Take resize control");
    root.layoutOwned.mockReturnValue(true);
    root.resizePane.mockRejectedValueOnce(new Error("This server does not support pane resizing."));
    await expect(resizeSessionPane(session(), "secondary", "up", 1)).rejects.toThrow("does not support");
    const resizing = resizeSessionPane(session(), "secondary", "up", 1);
    const request_id = root.resizePane.mock.lastCall![3];
    publishPaneResizeResult({ session: session(), attachment_id: "root", request_id, view: null, error: { code: "layout_lease_required", message: "Another client controls the layout." } });
    await expect(resizing).rejects.toThrow("Another client controls");
  });

  it("expires an unconfirmed resize without accepting unrelated view events", async () => {
    vi.useFakeTimers();
    try {
      const root = register("root", session()); root.layoutOwned.mockReturnValue(true);
      const resizing = resizeSessionPane(session(), "secondary", "right", 1);
      const failure = expect(resizing).rejects.toThrow("did not confirm");
      await vi.advanceTimersByTimeAsync(5000);
      await failure;
    } finally { vi.useRealTimers(); }
  });
  it("sends zoom through the session resize owner and waits for its matching acknowledgement", async () => {
    const root = register("root", session());
    const focused = register("focused", { ...session(), terminal_id: "secondary" });
    const unrelated = register("unrelated", session("other"));
    root.layoutOwned.mockReturnValue(true);
    unrelated.layoutOwned.mockReturnValue(true);
    const view: SessionView = {
      session_id: session().session_id, session_name: "shell", view_id: "view", revision: "2",
      canvas_size: session().terminal_size, zoomed_terminal_id: "secondary",
      layout: { kind: "terminal", terminal_id: "secondary" }, panes: [], terminals: [],
    };
    let confirmed = false;
    const changing = setSessionViewZoom(session(), "secondary", baseline).then((result) => { confirmed = true; return result; });
    expect(root.setViewZoom).toHaveBeenCalledExactlyOnceWith("secondary");
    expect(focused.setViewZoom).not.toHaveBeenCalled();
    expect(unrelated.setViewZoom).not.toHaveBeenCalled();
    publishSessionView({ session: session(), attachment_id: "focused", view });
    publishSessionView({ session: session(), attachment_id: "root", view: { ...view, zoomed_terminal_id: null } });
    // A delayed matching state from this owner's queue cannot confirm a new command.
    publishSessionView({ session: session(), attachment_id: "root", view: { ...view, revision: "0" } });
    publishSessionView({ session: session(), attachment_id: "root", view: { ...view, revision: "1" } });
    await Promise.resolve();
    expect(confirmed).toBe(false);
    publishSessionView({ session: session(), attachment_id: "root", view });
    expect(await changing).toEqual(view);
  });

  it("reports missing resize ownership, unsupported servers, and server rejection", async () => {
    const root = register("root", session());
    await expect(setSessionViewZoom(session(), "secondary", baseline)).rejects.toThrow("Take resize control");
    expect(root.setViewZoom).not.toHaveBeenCalled();
    root.layoutOwned.mockReturnValue(true);
    root.setViewZoom.mockRejectedValueOnce(new Error("This server does not support pane zoom."));
    await expect(setSessionViewZoom(session(), "secondary", baseline)).rejects.toThrow("does not support");
    const changing = setSessionViewZoom(session(), "secondary", baseline);
    publishSessionView({ session: session(), attachment_id: "root", error: "Another client controls the layout." });
    await expect(changing).rejects.toThrow("Another client controls");
  });

  it("reconnects exact root, background, and split-pane IDs once without touching other transports", async () => {
    const root = register("root", session("one"));
    const background = register("background", session("one"));
    const pane = register("pane", session("one"));
    const other = register("other", session("two"));
    expect(await reconnectComponentAttachments(["root", "background", "pane", "root"])).toEqual([
      { attachment_id: "root", replacement_attachment_id: "root-replacement", error: null },
      { attachment_id: "background", replacement_attachment_id: "background-replacement", error: null },
      { attachment_id: "pane", replacement_attachment_id: "pane-replacement", error: null },
    ]);
    for (const control of [root, background, pane]) expect(control.reconnect).toHaveBeenCalledOnce();
    expect(other.reconnect).not.toHaveBeenCalled();
  });

  it("reports vanished, replaced, and failed transports without claiming reconnect succeeded", async () => {
    const stale = register("stale", session("one"));
    const failed = register("failed", session("one"));
    stale.reconnect.mockResolvedValue(null);
    failed.reconnect.mockRejectedValue(new Error("Authentication canceled."));
    const results = await reconnectComponentAttachments(["vanished", "stale", "failed"]);
    expect(results.map((result) => result.replacement_attachment_id)).toEqual([null, null, null]);
    expect(results.every((result) => result.error !== null)).toBe(true);
    expect(results[2].error).toBe("Authentication canceled.");
  });

  it("resets all aliases of one verified environment while preserving colliding session IDs elsewhere", () => {
    const local = register("local", session());
    const active = register("active", session("primary", "environment-one"));
    const alias = register("alias", session("saved-alias", "environment-one"));
    const unmapped = register("unmapped", session("unmapped"));
    const unrelated = register("unrelated", session("other", "environment-two"));
    const event: ComponentSessionsReset = { scope: "remote", remote_id: "environment-one", host_ids: ["primary"], attachment_ids: ["unmapped"], session_ids: ["shared-session-id"] };
    expect(resetComponentAttachments(event)).toHaveLength(3);
    for (const control of [active, alias, unmapped]) expect(control.reset).toHaveBeenCalledOnce();
    expect(local.reset).not.toHaveBeenCalled();
    expect(unrelated.reset).not.toHaveBeenCalled();
    expect(componentResetMatches(event, session("disconnected-alias", "environment-one"), null)).toBe(true);
    expect(componentResetMatches(event, session("other", "environment-two"), null)).toBe(false);
  });

  it("resets only local owners for local daemon events and unregisters disposed panes", () => {
    const local = register("local", session());
    const remote = register("remote", session("one"));
    const disposed = register("disposed", session());
    stops.pop()!();
    expect(resetComponentAttachments({ scope: "local", host_ids: [], attachment_ids: [], session_ids: [] })).toEqual([session()]);
    expect(local.reset).toHaveBeenCalledOnce();
    expect(remote.reset).not.toHaveBeenCalled();
    expect(disposed.reset).not.toHaveBeenCalled();
  });
});
