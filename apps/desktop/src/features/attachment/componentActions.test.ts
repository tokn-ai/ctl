import { afterEach, describe, expect, it, vi } from "vitest";
import type { ComponentSessionsReset, SessionSummary } from "../../lib/types";
import { componentResetMatches, reconnectComponentAttachments, registerAttachmentControl, resetComponentAttachments } from "./componentActions";

const stops: (() => void)[] = [];
afterEach(() => { for (const stop of stops.splice(0)) stop(); });
const session = (host_id?: string, remote_id?: string): SessionSummary => ({
  target: host_id ? { kind: "ssh", host_id, destination: "sample", remote_info: remote_id ? { remote_id, agent_version: "0.1.0" } : undefined } : { kind: "local" },
  session_id: "shared-session-id", name: "shell", status: "running", terminal_size: { rows: 24, columns: 80, pixel_width: null, pixel_height: null }, next_sequence: "0",
});
function register(id: string, current: SessionSummary) {
  const control = { attachmentId: () => id, session: () => current, reconnect: vi.fn(async (): Promise<string | null> => `${id}-replacement`), reset: vi.fn() };
  stops.push(registerAttachmentControl(control));
  return control;
}

describe("component attachment actions", () => {
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
