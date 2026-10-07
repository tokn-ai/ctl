// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { LeaseStatus, SessionSummary } from "../../lib/types";
import { publishLayoutOwnerChange, registerAttachmentControl } from "./componentActions";
import { useResizeControl, useResizeWithWindow } from "./useResizeControl";

const stops: (() => void)[] = [];
afterEach(() => { cleanup(); for (const stop of stops.splice(0)) stop(); });
const session: SessionSummary = { target: { kind: "ssh", host_id: "primary", destination: "server", remote_info: { remote_id: "same-daemon", agent_version: "0.1.0" } },
  session_id: "shared", view_id: "view", name: "shell", status: "running", next_sequence: "0",
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null } };

function Fixture({ current = session, fallback = null }: { current?: SessionSummary; fallback?: LeaseStatus | null }) {
  const control = useResizeControl(current, fallback);
  const automatic = useResizeWithWindow(current, false);
  return <output>{control}:{String(automatic)}</output>;
}

describe("shared resize control subscription", () => {
  it("updates when a verified alias owner changes without changing the primary attachment state", () => {
    let owned = true;
    let automatic = false;
    render(<Fixture />);
    expect(screen.getByRole("status").textContent).toBe("unavailable:false");
    const alias: SessionSummary = { ...session, target: { kind: "ssh", host_id: "alias", destination: "another-route",
      remote_info: { remote_id: "same-daemon", agent_version: "0.1.0" } } };
    act(() => { stops.push(registerAttachmentControl({ attachmentId: () => "alias-owner", session: () => alias,
      layoutOwned: () => owned, layoutLease: () => ({ held: owned, owned_by_client: owned }), resizeWithWindow: () => automatic,
      requestResizeControl: async () => {}, toggleResizeWithWindow: async () => {}, enqueueViewportResize: () => {}, proposeViewportSize: () => null,
      reconnect: async () => null, reset: () => {}, setViewZoom: async () => {}, resizeDivider: async () => {}, resizePane: async () => {} })); });
    expect(screen.getByRole("status").textContent).toBe("owned:false");
    act(() => { automatic = true; publishLayoutOwnerChange(); });
    expect(screen.getByRole("status").textContent).toBe("owned:true");
    act(() => { owned = false; automatic = false; publishLayoutOwnerChange(); });
    expect(screen.getByRole("status").textContent).toBe("available:false");
  });

  it("uses the primary lease before registration while preserving external ownership", () => {
    render(<Fixture fallback={{ held: true, owned_by_client: false }} />);
    expect(screen.getByRole("status").textContent).toBe("held_elsewhere:false");
  });
});
