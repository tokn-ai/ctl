// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sessionView, sshConnectionStatus } from "../../lib/tauri";
import type { HostConnectionStatus, SessionSummary, SessionView, SshConnectionTarget } from "../../lib/types";
import { publishSessionView } from "../attachment/componentActions";
import { sessionKey } from "../targets/targets";
import { useSessionLayouts } from "./useSessionLayouts";

vi.mock("../../lib/tauri", () => ({ sessionView: vi.fn(), sshConnectionStatus: vi.fn() }));

const remote: SshConnectionTarget = {
  kind: "ssh", host_id: "remote", destination: "builder", method_id: "direct",
  remote_info: { remote_id: "remote-id", agent_version: "0.1.0" },
};
const connected: HostConnectionStatus = { state: "connected", method_names: ["Direct"], message: null };
const size = { columns: 80, rows: 24, pixel_width: null, pixel_height: null };

function session(overrides: Partial<SessionSummary> = {}): SessionSummary {
  return {
    session_id: "one", view_id: "view-one", terminal_id: "terminal-one", target: { kind: "local" },
    name: "Shell", status: "running", terminal_size: size, next_sequence: "1", last_seen_at_ms: 1000,
    ...overrides,
  };
}

function view(owner = session(), revision = "1"): SessionView {
  return {
    session_name: owner.name, session_id: owner.session_id, view_id: owner.view_id!, revision,
    canvas_size: size, zoomed_terminal_id: null, panes: [], terminals: [],
    layout: { kind: "split", axis: "horizontal", children: [
      { kind: "terminal", terminal_id: "left" }, { kind: "terminal", terminal_id: "right" },
    ] },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function setup(overrides: Partial<Parameters<typeof useSessionLayouts>[0]> = {}) {
  const initial: Parameters<typeof useSessionLayouts>[0] = {
    sessions: [session()], host_connections: new Map([["remote", connected]]), enabled: true, ...overrides,
  };
  return { ...renderHook(useSessionLayouts, { initialProps: initial }), initial };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(sessionView).mockResolvedValue(null);
  vi.mocked(sshConnectionStatus).mockResolvedValue({ connected: true, manually_disconnected: false });
});
afterEach(cleanup);

describe("session layout observations", () => {
  it("does not inspect restored, unreachable, unverified, paused, or unavailable sessions", () => {
    setup({ sessions: [
      session({ status: "unknown", view_id: undefined }),
      session({ session_id: "unreachable", status: "unreachable" }),
      session({ session_id: "unobserved", last_seen_at_ms: undefined }),
      session({ session_id: "unverified", target: { ...remote, remote_info: undefined } }),
      session({ session_id: "unavailable", target: { ...remote, unavailable: "VPN stopped" } }),
      session({ session_id: "paused", target: { ...remote, host_id: "paused" } }),
    ], host_connections: new Map([["remote", connected], ["paused", { ...connected, manually_disconnected: true }]]) });
    expect(sessionView).not.toHaveBeenCalled();
    expect(sshConnectionStatus).not.toHaveBeenCalled();
  });

  it("checks the exact remote route before fetching an inactive session layout", async () => {
    const owner = session({ target: remote });
    vi.mocked(sessionView).mockResolvedValue(view(owner));
    const { result } = setup({ sessions: [owner] });
    await waitFor(() => expect(result.current.layouts.get(sessionKey(owner))).toEqual(view(owner).layout));
    expect(sshConnectionStatus).toHaveBeenCalledExactlyOnceWith(remote);
    expect(sessionView).toHaveBeenCalledExactlyOnceWith(remote, { kind: "get", session_id: owner.session_id });
    expect(vi.mocked(sshConnectionStatus).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(sessionView).mock.invocationCallOrder[0]);
  });

  it("does not open a service when only another route of the host is connected", async () => {
    vi.mocked(sshConnectionStatus).mockResolvedValue({ connected: false, manually_disconnected: false });
    setup({ sessions: [session({ target: remote })] });
    await act(async () => undefined);
    expect(sshConnectionStatus).toHaveBeenCalledOnce();
    expect(sessionView).not.toHaveBeenCalled();
  });

  it("refreshes on observations and reconnects without querying for output or ordinary renders", async () => {
    const owner = session({ target: remote });
    const { rerender, initial } = setup({ sessions: [owner] });
    await waitFor(() => expect(sessionView).toHaveBeenCalledTimes(1));
    rerender({ ...initial, sessions: [{ ...owner, next_sequence: "999", name: "Updated title" }] });
    await act(async () => undefined);
    expect(sessionView).toHaveBeenCalledTimes(1);
    rerender({ ...initial, sessions: [{ ...owner, last_seen_at_ms: 2000 }] });
    await waitFor(() => expect(sessionView).toHaveBeenCalledTimes(2));
    rerender({ ...initial, host_connections: new Map([["remote", { ...connected, state: "disconnected" }]]) });
    rerender(initial);
    await waitFor(() => expect(sessionView).toHaveBeenCalledTimes(3));
  });

  it("uses attached view events without supplemental lookups on heartbeat timestamps", async () => {
    const owner = session();
    const { result, initial, rerender } = setup({ attached_session_keys: new Set([sessionKey(owner)]) });
    const newer = view(owner, "4");
    act(() => publishSessionView({ session: owner, attachment_id: "attachment", view: newer }));
    expect(result.current.layouts.get(sessionKey(owner))).toEqual(newer.layout);
    const older = { ...view(owner, "3"), layout: { kind: "terminal" as const, terminal_id: "older" } };
    act(() => publishSessionView({ session: owner, attachment_id: "attachment", view: older }));
    expect(result.current.layouts.get(sessionKey(owner))).toEqual(newer.layout);
    rerender({ ...initial, sessions: [{ ...owner, last_seen_at_ms: 2000 }] });
    await act(async () => undefined);
    expect(sessionView).not.toHaveBeenCalled();
  });

  it("rejects wrong session/view identities and never shares layouts across host aliases", () => {
    const first = session({ target: remote });
    const alias = session({ target: { ...remote, host_id: "alias" } });
    const { result } = setup({ sessions: [first, alias], attached_session_keys: new Set([sessionKey(first), sessionKey(alias)]) });
    act(() => result.current.observeView(first, { ...view(first), session_id: "other" }));
    act(() => result.current.observeView(first, { ...view(first), view_id: "retired-view" }));
    expect(result.current.layouts.size).toBe(0);
    act(() => result.current.observeView(first, view(first)));
    expect(result.current.layouts.has(sessionKey(first))).toBe(true);
    expect(result.current.layouts.has(sessionKey(alias))).toBe(false);
  });

  it("keeps the first attachment view until its runtime identity is adopted", () => {
    const restored = session({ status: "unknown", view_id: undefined });
    const attached = session();
    const { result, rerender, initial } = setup({ sessions: [restored], attached_session_keys: new Set([sessionKey(restored)]) });
    act(() => publishSessionView({ session: attached, attachment_id: "new-attachment", view: view(attached) }));
    expect(result.current.layouts.get(sessionKey(restored))).toEqual(view(attached).layout);
    rerender({ ...initial, sessions: [attached] });
    expect(result.current.layouts.has(sessionKey(attached))).toBe(true);
    rerender({ ...initial, sessions: [restored] });
    expect(result.current.layouts.size).toBe(0);
  });

  it("does not let an older lookup overwrite a newer attachment event", async () => {
    const pending = deferred<SessionView | null>();
    vi.mocked(sessionView).mockReturnValueOnce(pending.promise);
    const { result } = setup();
    const authoritative = { ...view(session(), "7"), layout: { kind: "terminal" as const, terminal_id: "survivor" } };
    act(() => publishSessionView({ session: session(), attachment_id: "attached", view: authoritative }));
    await act(async () => pending.resolve(view(session(), "6")));
    expect(result.current.layouts.get(sessionKey(session()))).toEqual(authoritative.layout);
  });

  it("rejects late requests after removal and after a view is replaced", async () => {
    const pending = deferred<SessionView | null>();
    vi.mocked(sessionView).mockReturnValueOnce(pending.promise);
    const { result, rerender, initial } = setup();
    rerender({ ...initial, sessions: [] });
    rerender({ ...initial, sessions: [session({ view_id: "replacement" })] });
    await act(async () => pending.resolve(view()));
    expect(result.current.layouts.size).toBe(0);
    act(() => result.current.observeView(session({ view_id: "replacement" }), view(session({ view_id: "replacement" }))));
    expect(result.current.layouts.size).toBe(1);
    rerender({ ...initial, sessions: [] });
    expect(result.current.layouts.size).toBe(0);
  });

  it("abandons pending route checks when the host pauses or its route changes", async () => {
    const pending = deferred<{ connected: boolean; manually_disconnected: boolean }>();
    vi.mocked(sshConnectionStatus).mockReturnValueOnce(pending.promise);
    const { result, rerender, initial } = setup({ sessions: [session({ target: remote })] });
    rerender({ ...initial, host_connections: new Map([["remote", { ...connected, state: "disconnecting" }]]) });
    await act(async () => pending.resolve({ connected: true, manually_disconnected: false }));
    expect(sessionView).not.toHaveBeenCalled();
    const old = session({ target: remote });
    rerender({ ...initial, sessions: [session({ target: { ...remote, destination: "replacement" } })] });
    act(() => result.current.observeView(old, view(old)));
    expect(result.current.layouts.size).toBe(0);
  });

  it("bounds the queue and starts the next inactive lookup only when a slot is free", async () => {
    const owners = Array.from({ length: 6 }, (_, index) => session({ session_id: `session-${index}`, view_id: `view-${index}` }));
    const pending = owners.map(() => deferred<SessionView | null>());
    vi.mocked(sessionView).mockImplementation((_target, action) => pending[Number(action.kind === "get" ? action.session_id.split("-")[1] : -1)].promise);
    setup({ sessions: owners });
    expect(sessionView).toHaveBeenCalledTimes(4);
    await act(async () => pending[0].resolve(view(owners[0])));
    expect(sessionView).toHaveBeenCalledTimes(5);
    await act(async () => pending[1].resolve(view(owners[1])));
    expect(sessionView).toHaveBeenCalledTimes(6);
    await act(async () => pending.slice(2).forEach((item) => item.resolve(null)));
  });

  it("keeps failures silent and retries only with new evidence", async () => {
    vi.mocked(sessionView).mockRejectedValue(new Error("Daemon disappeared"));
    const { rerender, initial, result } = setup();
    await act(async () => undefined);
    rerender({ ...initial });
    await act(async () => undefined);
    expect(sessionView).toHaveBeenCalledOnce();
    expect(result.current.layouts.size).toBe(0);
    rerender({ ...initial, enabled: false });
    rerender({ ...initial, enabled: true, sessions: [session({ last_seen_at_ms: 2000 })] });
    await act(async () => undefined);
    expect(sessionView).toHaveBeenCalledTimes(2);
  });
});
