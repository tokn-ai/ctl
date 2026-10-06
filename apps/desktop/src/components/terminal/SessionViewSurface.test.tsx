// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttachmentViewState, SessionSummary, SessionView } from "../../lib/types";
import { sessionKey } from "../../features/targets/targets";
import { SessionViewSurface } from "./SessionViewSurface";
import type { AppCommand } from "../../features/commands/types";
import { searchCommands } from "../../features/commands/commandSearch";
import type { XtermRenderer } from "../../features/terminal/XtermRenderer";
import { initialAttachmentState } from "../../features/attachment/attachmentState";
import { NotificationProvider, useNotificationEnvironment } from "../../features/notifications/NotificationContext";
import { NotificationStore } from "../../features/notifications/NotificationStore";
import type { AttachmentNotifications } from "../../features/notifications/AttachmentNotifications";
import { publishPaneResizeResult, publishSessionView, registerAttachmentControl } from "../../features/attachment/componentActions";

const mocks = vi.hoisted(() => ({ request: vi.fn(), zoom: vi.fn(), resize: vi.fn(), divider: vi.fn(), mount: vi.fn(), unmount: vi.fn(), connect: vi.fn(), reconnect: vi.fn(), detach: vi.fn(), input: vi.fn(), attachment_state: null as AttachmentViewState | null, mounted_inputs: [] as ((data: Uint8Array) => void)[] }));
vi.mock("../../lib/tauri", () => ({ sessionView: mocks.request }));
vi.mock("../../features/attachment/useAttachment", () => ({ useAttachment: () => ({
  state: mocks.attachment_state ?? { phase: "attached", applied_sequence: "0", input_lease: { owned_by_client: true } },
  connect: mocks.connect, reconnect: mocks.reconnect, detach: mocks.detach, handleInput: mocks.input, toggleInputLease: vi.fn(), toggleResizeWithWindow: vi.fn(),
}) }));
vi.mock("./TerminalSurface", async () => {
  const { useEffect } = await import("react");
  const renderer = {};
  return { TerminalSurface: ({ onReady, onInput, ended_message, on_dismiss }: { onReady(renderer: unknown): void; onInput(data: Uint8Array): void; ended_message?: string | null; on_dismiss?(): void }) => {
    useEffect(() => {
      mocks.mount();
      mocks.mounted_inputs.push(onInput);
      onReady(renderer);
      return () => { mocks.unmount(); onReady(null); };
    }, []);
    return <div data-testid="terminal-surface" className="terminal-container"><textarea aria-label="Terminal input" />{ended_message && <button onClick={on_dismiss}>Dismiss ended pane</button>}</div>;
  } };
});

const session: SessionSummary = { target: { kind: "local" }, session_id: "root", view_id: "view", terminal_id: "a", name: "Root", status: "running", next_sequence: "0", terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null } };
const terminal = (terminal_id: string) => ({ terminal_id, name: terminal_id, terminal_size: session.terminal_size, next_sequence: "0" });
const initial: SessionView = { session_id: "root", session_name: "Root", view_id: "view", revision: "0", canvas_size: session.terminal_size, zoomed_terminal_id: null, panes: [{ terminal_id: "a", left: 0, top: 0, columns: 80, rows: 24 }], layout: { kind: "terminal", terminal_id: "a" }, terminals: [terminal("a")] };
const split: SessionView = { ...initial, revision: "1", panes: [{ terminal_id: "a", left: 0, top: 0, columns: 40, rows: 24 }, { terminal_id: "b", left: 41, top: 0, columns: 39, rows: 24 }], layout: { kind: "split", axis: "horizontal", children: [{ kind: "terminal", terminal_id: "a" }, { kind: "terminal", terminal_id: "b" }] }, terminals: [terminal("a"), terminal("b")] };
const props = () => ({ session, on_promoted: vi.fn(), on_select_terminal: vi.fn(), phase: "attached" as const, hasSession: true, has_cached_content: true, onInput: vi.fn(), onReady: vi.fn() });
let stop_control: () => void;
beforeEach(() => {
  vi.stubGlobal("PointerEvent", class extends MouseEvent { readonly pointerId = 1; readonly isPrimary = true; });
  Object.defineProperty(HTMLElement.prototype, "setPointerCapture", { configurable: true, value() {} });
  mocks.zoom.mockImplementation(async (terminal_id: string | null) => {
    publishSessionView({ session, attachment_id: "primary-owner", view: { ...split, revision: terminal_id ? "2" : "3", zoomed_terminal_id: terminal_id } });
  });
  mocks.resize.mockImplementation(async (_terminal_id: string, _direction: string, _amount: number, request_id: string) => {
    publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id, view: {
      ...split, revision: "2", layout: { ...split.layout, ...{ weights: [2, 1] } },
      panes: [{ ...split.panes[0], columns: 53 }, { ...split.panes[1], left: 54, columns: 26 }],
    }, error: null });
  });
  mocks.divider.mockImplementation(async () => {});
  stop_control = registerAttachmentControl({
    attachmentId: () => "primary-owner", session: () => session,
    layoutOwned: () => true, setViewZoom: mocks.zoom, resizePane: mocks.resize, resizeDivider: mocks.divider,
    layoutLease: () => ({ held: true, owned_by_client: true }), requestResizeControl: async () => {},
    resizeWithWindow: () => false, toggleResizeWithWindow: async () => {},
    enqueueViewportResize: () => {}, proposeViewportSize: () => null,
    reconnect: async () => null, reset: () => {},
  });
});

afterEach(() => { cleanup(); stop_control(); Reflect.deleteProperty(HTMLElement.prototype, "setPointerCapture"); vi.unstubAllGlobals(); vi.clearAllMocks(); mocks.attachment_state = null; mocks.mounted_inputs = []; vi.useRealTimers(); });

describe("session compositor", () => {
  it("drags the server divider without remounting panes or blocking terminal input, and keeps keyboard resize serialized after Escape", async () => {
    mocks.request.mockResolvedValue(split);
    const actions = props();
    const prefix_settings = { document: { schema_version: 1 as const, overrides: [] }, bindings: new Map(), platform: "other" as const };
    render(<SessionViewSurface {...actions} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const inputs = screen.getAllByLabelText("Terminal input");
    act(() => inputs[1].focus());
    const handle = screen.getByRole("separator");
    fireEvent.pointerDown(handle, { button: 0, clientX: 324, clientY: 100 });
    fireEvent.pointerMove(handle, { clientX: 364, clientY: 100 });
    await waitFor(() => expect(mocks.divider).toHaveBeenCalledTimes(1));
    expect(mocks.divider).toHaveBeenCalledExactlyOnceWith({ view_id: "view", expected_revision: "1", split_path: [], boundary: 0, position: 45 }, expect.any(String));
    expect(inputs[1].closest<HTMLElement>(".view-pane")?.style.left).toBe("328px");
    expect(document.activeElement).toBe(inputs[1]);
    fireEvent.keyDown(window, { key: "Escape" });
    fireEvent.keyDown(inputs[1], { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(inputs[1], { key: "ArrowRight", ctrlKey: true });
    expect(mocks.resize).not.toHaveBeenCalled();
    const input = new Uint8Array([120]);
    act(() => mocks.mounted_inputs[0](input));
    expect(actions.onInput).toHaveBeenCalledExactlyOnceWith(input);
    const request_id = mocks.divider.mock.lastCall![1];
    const next: SessionView = { ...split, revision: "2", layout: { kind: "split", axis: "horizontal", weights: [45, 34], children: [{ kind: "terminal", terminal_id: "a" }, { kind: "terminal", terminal_id: "b" }] },
      panes: [{ ...split.panes[0], columns: 45 }, { ...split.panes[1], left: 46, columns: 34 }] };
    await act(async () => publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id, view: next, error: null }));
    expect(inputs[1].closest<HTMLElement>(".view-pane")?.style.left).toBe("368px");
    expect(screen.getAllByLabelText("Terminal input")).toEqual(inputs);
    expect(document.activeElement).toBe(inputs[1]);
    expect(mocks.mount).toHaveBeenCalledTimes(2);
    expect(mocks.unmount).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Split right" }).hasAttribute("disabled")).toBe(false);
  });

  it("reads a synchronous newer view before React renders a batched observer update and divider ACK", async () => {
    mocks.request.mockResolvedValue(split);
    render(<SessionViewSurface {...props()} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const handle = screen.getByRole("separator");
    fireEvent.pointerDown(handle, { button: 0, clientX: 324 });
    fireEvent.pointerMove(handle, { clientX: 332 });
    await waitFor(() => expect(mocks.divider).toHaveBeenCalledTimes(1));
    fireEvent.pointerMove(handle, { clientX: 340 });
    const request_id = mocks.divider.mock.lastCall![1];
    await act(async () => {
      publishSessionView({ session, attachment_id: "observer", view: { ...split, revision: "9" } });
      publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id, view: { ...split, revision: "2" }, error: null });
    });
    expect(mocks.divider).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Split right" }).hasAttribute("disabled")).toBe(false);
  });

  it("rejects a drag on an old rendered handle when topology changes in the same batch", async () => {
    mocks.request.mockResolvedValue(split);
    render(<SessionViewSurface {...props()} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const old_handle = screen.getByRole("separator");
    const vertical: SessionView = { ...split, revision: "2", layout: { kind: "split", axis: "vertical", children: [{ kind: "terminal", terminal_id: "a" }, { kind: "terminal", terminal_id: "b" }] },
      panes: [{ ...split.panes[0], columns: 80, rows: 12 }, { ...split.panes[1], left: 0, top: 13, columns: 80, rows: 11 }] };
    await act(async () => {
      publishSessionView({ session, attachment_id: "observer", view: vertical });
      fireEvent.pointerDown(old_handle, { button: 0, clientX: 324, clientY: 100 });
      fireEvent.pointerMove(old_handle, { clientX: 332, clientY: 100 });
    });
    expect(mocks.divider).not.toHaveBeenCalled();
    const new_handle = screen.getByRole("separator");
    expect(new_handle.getAttribute("aria-orientation")).toBe("horizontal");
    fireEvent.pointerDown(new_handle, { button: 0, clientX: 100, clientY: 200 });
    fireEvent.pointerMove(new_handle, { clientX: 100, clientY: 216 });
    await waitFor(() => expect(mocks.divider).toHaveBeenCalledTimes(1));
    expect(mocks.divider).toHaveBeenCalledExactlyOnceWith({ view_id: "view", expected_revision: "2", split_path: [], boundary: 0, position: 13 }, expect.any(String));
    await act(async () => publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id: mocks.divider.mock.lastCall![1], view: { ...vertical, revision: "3" }, error: null }));
  });

  it.each(["primary", "secondary", "disconnect"])("cancels pointer work on %s ending or interruption and drops its pending target", async (ending) => {
    mocks.request.mockResolvedValue(split);
    const actions = props();
    const mounted = render(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const handle = screen.getByRole("separator");
    fireEvent.pointerDown(handle, { button: 0, clientX: 324 });
    fireEvent.pointerMove(handle, { clientX: 332 });
    await waitFor(() => expect(mocks.divider).toHaveBeenCalledTimes(1));
    fireEvent.pointerMove(handle, { clientX: 340 });
    if (ending === "secondary") mocks.attachment_state = { ...initialAttachmentState(), session: { ...session, terminal_id: "b" }, phase: "ended" };
    mounted.rerender(<SessionViewSurface {...actions} phase={ending === "primary" ? "ended" : ending === "disconnect" ? "reconnecting" : "attached"} />);
    const request_id = mocks.divider.mock.lastCall![1];
    await act(async () => publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id, view: null,
      error: { code: "session_not_found", message: "The terminal ended during resize." } }));
    expect(mocks.divider).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(mocks.unmount).not.toHaveBeenCalled();
  });
  it("resizes focused secondary panes through ownership and renders weighted server geometry without remounting", async () => {
    mocks.request.mockResolvedValue(split);
    const commands = vi.fn();
    const prefix_settings = { document: { schema_version: 1 as const, overrides: [] }, bindings: new Map(), platform: "other" as const };
    render(<SessionViewSurface {...props()} prefix_settings={prefix_settings} on_pane_commands={commands} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const second = screen.getAllByLabelText("Terminal input")[1];
    act(() => second.focus());
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "ArrowLeft", ctrlKey: true });
    await waitFor(() => expect(mocks.resize).toHaveBeenCalledWith("b", "left", 1, expect.any(String)));
    await waitFor(() => expect(second.closest<HTMLElement>(".view-pane")?.style.width).toBe("208px"));
    expect(second.closest<HTMLElement>(".view-pane")?.style.left).toBe("432px");
    expect(mocks.mount).toHaveBeenCalledTimes(2);
    expect(mocks.unmount).not.toHaveBeenCalled();
    const pane_commands = commands.mock.lastCall![0] as AppCommand[];
    expect(pane_commands.filter((command) => command.id.startsWith("pane.resize_"))).toHaveLength(4);
    await act(async () => pane_commands.find((command) => command.id === "pane.resize_down")!.run());
    expect(mocks.resize).toHaveBeenLastCalledWith("b", "down", 1, expect.any(String));
  });

  it("queues repeated resize deltas while awaiting an exact acknowledgement and keeps zoom on rejection", async () => {
    mocks.request.mockResolvedValue({ ...split, zoomed_terminal_id: "b" });
    mocks.resize.mockImplementation(async () => {});
    const prefix_settings = { document: { schema_version: 1 as const, overrides: [] }, bindings: new Map(), platform: "other" as const };
    render(<SessionViewSurface {...props()} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const second = screen.getAllByLabelText("Terminal input")[1];
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "ArrowRight", altKey: true });
    fireEvent.keyDown(second, { key: "ArrowRight", altKey: true, repeat: true });
    fireEvent.keyDown(second, { key: "ArrowRight", altKey: true, repeat: true });
    expect(mocks.resize).toHaveBeenCalledTimes(1);
    const first_request = mocks.resize.mock.lastCall![3];
    await act(async () => publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id: first_request, view: { ...split, revision: "2" }, error: null }));
    await waitFor(() => expect(mocks.resize).toHaveBeenCalledTimes(2));
    expect(mocks.resize).toHaveBeenLastCalledWith("b", "right", 10, expect.any(String));
    const second_request = mocks.resize.mock.lastCall![3];
    await act(async () => {
      publishSessionView({ session, attachment_id: "primary-owner", view: { ...split, revision: "3", zoomed_terminal_id: "b" } });
      publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id: second_request, view: null, error: { code: "layout_lease_required", message: "Take resize control." } });
    });
    expect(screen.getByRole("alert").textContent).toBe("Take resize control.");
    expect(second.closest<HTMLElement>(".view-pane")?.style.width).toBe("640px");
    expect(mocks.unmount).not.toHaveBeenCalled();
  });
  it.each(["primary", "secondary"])("cancels queued resize work when the %s pane ends and retains its final screen", async (ended) => {
    mocks.request.mockResolvedValue(split);
    mocks.resize.mockImplementation(async () => {});
    const prefix_settings = { document: { schema_version: 1 as const, overrides: [] }, bindings: new Map(), platform: "other" as const };
    const actions = props();
    const mounted = render(<SessionViewSurface {...actions} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const input = screen.getAllByLabelText("Terminal input")[0];
    fireEvent.keyDown(input, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(input, { key: "ArrowRight", ctrlKey: true });
    fireEvent.keyDown(input, { key: "ArrowRight", ctrlKey: true, repeat: true });
    expect(mocks.resize).toHaveBeenCalledTimes(1);
    const request_id = mocks.resize.mock.lastCall![3];
    if (ended === "secondary") mocks.attachment_state = { ...initialAttachmentState(), session: { ...session, terminal_id: "b" }, phase: "ended" };
    mounted.rerender(<SessionViewSurface {...actions} phase={ended === "primary" ? "ended" : "attached"} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Split right" }).hasAttribute("disabled")).toBe(true));
    await act(async () => publishPaneResizeResult({ session, attachment_id: "primary-owner", request_id, view: null, error: { code: "session_not_found", message: "The terminal ended during resize." } }));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getAllByLabelText("Terminal input")[0]).toBe(input);
    expect(mocks.resize).toHaveBeenCalledTimes(1);
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

  it.each([false, true])("isolates resize workers across session changes and stale results (rejected=%s)", async (rejected) => {
    const other_session = { ...session, session_id: "other", view_id: "other-view" };
    const other_view = { ...split, session_id: "other", view_id: "other-view" };
    mocks.request.mockImplementation((_target, action) => Promise.resolve(action.session_id === "other" ? other_view : split));
    mocks.resize.mockImplementation(async () => {});
    const resize_other = vi.fn(async () => {});
    const stop_other = registerAttachmentControl({ attachmentId: () => "other-owner", session: () => other_session,
      layoutLease: () => ({ held: true, owned_by_client: true }), requestResizeControl: async () => {},
      resizeWithWindow: () => false, toggleResizeWithWindow: async () => {}, enqueueViewportResize: () => {}, proposeViewportSize: () => null,
      layoutOwned: () => true, setViewZoom: async () => {}, resizePane: resize_other, resizeDivider: vi.fn(async () => {}), reconnect: async () => null, reset: () => {} });
    try {
      const prefix_settings = { document: { schema_version: 1 as const, overrides: [] }, bindings: new Map(), platform: "other" as const };
      const actions = props();
      const mounted = render(<SessionViewSurface {...actions} prefix_settings={prefix_settings} />);
      await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
      let input = screen.getAllByLabelText("Terminal input")[0];
      fireEvent.keyDown(input, { key: "b", code: "KeyB", ctrlKey: true });
      fireEvent.keyDown(input, { key: "ArrowRight", ctrlKey: true });
      const stale_id = mocks.resize.mock.lastCall![3];
      mounted.rerender(<SessionViewSurface {...actions} session={other_session} prefix_settings={prefix_settings} />);
      await waitFor(() => expect(mocks.request).toHaveBeenLastCalledWith(other_session.target, { kind: "get", session_id: "other" }));
      input = screen.getAllByLabelText("Terminal input")[0];
      await waitFor(() => expect(input.closest<HTMLElement>(".view-pane")?.style.width).toBe("320px"));
      fireEvent.keyDown(input, { key: "b", code: "KeyB", ctrlKey: true });
      fireEvent.keyDown(input, { key: "ArrowLeft", altKey: true });
      await waitFor(() => expect(resize_other).toHaveBeenCalledTimes(1));
      const fresh_id = (resize_other.mock.lastCall as unknown as [string, string, number, string])[3];
      await act(async () => publishPaneResizeResult(rejected
        ? { session, attachment_id: "primary-owner", request_id: stale_id, view: null, error: { code: "invalid_request", message: "Stale resize failed." } }
        : { session, attachment_id: "primary-owner", request_id: stale_id, view: { ...split, revision: "99" }, error: null }));
      expect(screen.queryByText("Stale resize failed.")).toBeNull();
      fireEvent.keyDown(input, { key: "ArrowLeft", altKey: true, repeat: true });
      await act(async () => publishPaneResizeResult({ session: other_session, attachment_id: "other-owner", request_id: fresh_id, view: { ...other_view, revision: "2" }, error: null }));
      await waitFor(() => expect(resize_other).toHaveBeenCalledTimes(2));
      const next_id = (resize_other.mock.lastCall as unknown as [string, string, number, string])[3];
      await act(async () => publishPaneResizeResult({ session: other_session, attachment_id: "other-owner", request_id: next_id, view: { ...other_view, revision: "3" }, error: null }));
      expect(screen.queryByRole("alert")).toBeNull();
    } finally { stop_other(); }
  });

  it("reports hidden split-pane failures with a targeted retry and keeps them in history after close", async () => {
    const store = new NotificationStore();
    let registry!: AttachmentNotifications;
    function Registry() { registry = useNotificationEnvironment()!.attachments; return null; }
    const other = { ...session, session_id: "other", terminal_id: "other-a", view_id: "other-view" };
    const other_view = { ...initial, session_id: "other", view_id: "other-view", panes: [{ ...initial.panes[0], terminal_id: "other-a" }], terminals: [terminal("other-a")] };
    mocks.request.mockImplementation((_target, action) => Promise.resolve(action.session_id === "other" ? other_view : split));
    const open_keys = new Set([sessionKey(session), sessionKey(other)]);
    const actions = props();
    const view = (active: SessionSummary, keys = open_keys) => <NotificationProvider store={store}>
      <Registry /><SessionViewSurface {...actions} session={active} open_session_keys={keys} />
    </NotificationProvider>;
    const mounted = render(view(session));
    await waitFor(() => expect(mocks.connect).toHaveBeenCalledOnce());
    const secondary = screen.getAllByLabelText("Terminal input")[1];
    mounted.rerender(view(other));
    await waitFor(() => expect(secondary.closest<HTMLElement>(".view-pane")?.hidden).toBe(true));
    mocks.attachment_state = { ...initialAttachmentState(), session: { ...session, terminal_id: "b" }, phase: "error", error_code: "ssh_authentication_required", message: "Secondary authentication failed" };
    mounted.rerender(view(other));
    await waitFor(() => expect(store.snapshot().entries).toHaveLength(1));
    expect(screen.queryByText("Secondary authentication failed")).toBeNull();
    expect(store.snapshot().entries[0]).toMatchObject({ title: "Session connection failed", source: "local · Root · b" });
    const owner = store.snapshot().entries[0].actions![0].args!.value;
    await act(async () => registry.reconnect(owner));
    expect(mocks.reconnect).toHaveBeenCalledOnce();
    expect(actions.on_select_terminal).not.toHaveBeenCalled();
    mounted.rerender(view(other, new Set([sessionKey(other)])));
    await waitFor(() => expect(registry.canReconnect(owner)).toBe(false));
    expect(store.snapshot().entries).toHaveLength(1);
    expect(store.snapshot().entries[0].actions).toEqual([]);
  });

  it("blocks all pane input and prefix actions while disabled, then resumes without reconnecting", async () => {
    mocks.request.mockResolvedValue(split);
    const actions = props();
    const prefix_settings = { document: { schema_version: 1 as const, overrides: [] }, bindings: new Map(), platform: "other" as const };
    const mounted = render(<SessionViewSurface {...actions} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(mocks.connect).toHaveBeenCalledOnce());
    const [primary_input, secondary_input] = mocks.mounted_inputs;
    const bytes = new Uint8Array([97]);
    primary_input(bytes);
    secondary_input(bytes);
    expect(actions.onInput).toHaveBeenCalledExactlyOnceWith(bytes);
    expect(mocks.input).toHaveBeenCalledExactlyOnceWith(bytes);
    actions.onInput.mockClear();
    mocks.input.mockClear();

    const [first, second] = screen.getAllByLabelText("Terminal input");
    act(() => second.focus());
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    expect(screen.getByText("Ctrl+B", { selector: "strong" })).toBeTruthy();
    mounted.rerender(<SessionViewSurface {...actions} prefix_settings={prefix_settings} input_enabled={false} />);
    primary_input(bytes);
    secondary_input(bytes);
    const requests = mocks.request.mock.calls.length;
    for (const input of [first, second]) {
      fireEvent.keyDown(input, { key: "b", code: "KeyB", ctrlKey: true });
      fireEvent.keyDown(input, { key: "b", code: "KeyB", ctrlKey: true });
      fireEvent.keyDown(input, { key: "v", code: "KeyV" });
    }
    expect(screen.queryByText("Ctrl+B", { selector: "strong" })).toBeNull();
    expect(actions.onInput).not.toHaveBeenCalled();
    expect(mocks.input).not.toHaveBeenCalled();
    expect(mocks.request).toHaveBeenCalledTimes(requests);

    mounted.rerender(<SessionViewSurface {...actions} prefix_settings={prefix_settings} />);
    primary_input(bytes);
    secondary_input(bytes);
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    expect(actions.onInput).toHaveBeenCalledExactlyOnceWith(bytes);
    expect(mocks.input).toHaveBeenNthCalledWith(1, bytes);
    expect(mocks.input).toHaveBeenNthCalledWith(2, new Uint8Array([2]));
    expect(mocks.mount).toHaveBeenCalledTimes(2);
    expect(mocks.unmount).not.toHaveBeenCalled();
    expect(mocks.connect).toHaveBeenCalledOnce();
    expect(mocks.detach).not.toHaveBeenCalled();
  });

  it("blocks renderer-held input and prefix events from cached secondary panes while hidden", async () => {
    const other = { ...session, session_id: "other", terminal_id: "other-a", view_id: "other-view" };
    const other_view = { ...initial, session_id: "other", view_id: "other-view", panes: [{ ...initial.panes[0], terminal_id: "other-a" }], terminals: [terminal("other-a")] };
    mocks.request.mockImplementation((_target, action) => Promise.resolve(action.session_id === "other" ? other_view : split));
    const open_session_keys = new Set([sessionKey(session), sessionKey(other)]);
    const prefix_settings = { document: { schema_version: 1 as const, overrides: [] }, bindings: new Map(), platform: "other" as const };
    const actions = props();
    const mounted = render(<SessionViewSurface {...actions} open_session_keys={open_session_keys} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(mocks.connect).toHaveBeenCalledOnce());
    const secondary_input = mocks.mounted_inputs[1];
    const secondary = screen.getAllByLabelText("Terminal input")[1];
    mounted.rerender(<SessionViewSurface {...actions} session={other} open_session_keys={open_session_keys} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(secondary.closest<HTMLElement>(".view-pane")?.hidden).toBe(true));
    secondary_input(new Uint8Array([97]));
    fireEvent.keyDown(secondary, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(secondary, { key: "b", code: "KeyB", ctrlKey: true });
    expect(mocks.input).not.toHaveBeenCalled();
    expect(actions.onInput).not.toHaveBeenCalled();
    expect(screen.queryByText("Ctrl+B", { selector: "strong" })).toBeNull();
    mounted.rerender(<SessionViewSurface {...actions} open_session_keys={open_session_keys} prefix_settings={prefix_settings} />);
    await waitFor(() => expect(secondary.closest<HTMLElement>(".view-pane")?.hidden).toBe(false));
    secondary_input(new Uint8Array([98]));
    expect(mocks.input).toHaveBeenCalledExactlyOnceWith(new Uint8Array([98]));
    expect(mocks.connect).toHaveBeenCalledOnce();
    expect(mocks.unmount).not.toHaveBeenCalled();
    expect(mocks.detach).not.toHaveBeenCalled();
  });

  it("updates a single pane's active outline when rendered cell dimensions change", async () => {
    mocks.request.mockResolvedValue(initial);
    let measure!: (cell: { width: number; height: number }) => void;
    const stop = vi.fn();
    const renderer = {
      setViewport: vi.fn(),
      observeCellDimensions: vi.fn((callback) => { measure = callback; return stop; }),
    } as unknown as XtermRenderer;
    const mounted = render(<SessionViewSurface {...props()} renderer={renderer} />);
    await waitFor(() => expect(screen.getByLabelText("Terminal input").closest<HTMLElement>(".view-pane")?.style.width).toBe("640px"));
    act(() => measure({ width: 10, height: 20 }));
    const pane = screen.getByLabelText("Terminal input").closest<HTMLElement>(".view-pane")!;
    expect(pane.dataset.active).toBe("true");
    expect(pane.style.width).toBe("800px");
    expect(pane.style.height).toBe("480px");
    act(() => measure({ width: 7, height: 15 }));
    expect(pane.style.width).toBe("560px");
    expect(pane.style.height).toBe("360px");
    mounted.unmount();
    expect(stop).toHaveBeenCalled();
  });

  it("keeps split pane channels mounted while another opened session is selected", async () => {
    const other = { ...session, session_id: "other", terminal_id: "other-a", view_id: "other-view" };
    const other_view = { ...initial, session_id: "other", view_id: "other-view", panes: [{ ...initial.panes[0], terminal_id: "other-a" }], terminals: [terminal("other-a")] };
    mocks.request.mockImplementation((_target, action) => Promise.resolve(action.session_id === "other" ? other_view : split));
    const open_session_keys = new Set([sessionKey(session), sessionKey(other)]);
    const mounted = render(<SessionViewSurface {...props()} open_session_keys={open_session_keys} />);
    await waitFor(() => expect(mocks.connect).toHaveBeenCalledTimes(1));
    mounted.rerender(<SessionViewSurface {...props()} session={other} open_session_keys={open_session_keys} />);
    await waitFor(() => expect(mocks.request).toHaveBeenCalledWith(other.target, { kind: "get", session_id: "other" }));
    expect(mocks.detach).not.toHaveBeenCalled();
    mounted.rerender(<SessionViewSurface {...props()} open_session_keys={open_session_keys} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    expect(mocks.connect).toHaveBeenCalledTimes(1);
    expect(mocks.detach).not.toHaveBeenCalled();
    mounted.rerender(<SessionViewSurface {...props()} session={other} open_session_keys={new Set([sessionKey(other)])} />);
    await waitFor(() => expect(mocks.detach).toHaveBeenCalled());
  });

  it("keeps the cached layout and border dimensions while disconnected", async () => {
    mocks.request.mockResolvedValue(split);
    const actions = props();
    const mounted = render(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    mounted.rerender(<SessionViewSurface {...actions} phase="disconnected" />);
    const panes = screen.getAllByLabelText("Terminal input").map((input) => input.closest<HTMLElement>(".view-pane")!);
    expect(panes.map((pane) => pane.style.width)).toEqual(["320px", "312px"]);
    expect(panes.map((pane) => pane.style.height)).toEqual(["384px", "384px"]);
  });

  it("bounds a missing session's fallback canvas and dismisses without a network request", async () => {
    const actions = { ...props(), on_dismiss: vi.fn() };
    render(<SessionViewSurface {...actions} phase="disconnected" ended_message="Session no longer exists" />);
    const canvas = screen.getByLabelText("Terminal input").closest(".view-panes") as HTMLElement;
    expect(canvas.style.width).toBe("640px");
    expect(canvas.style.height).toBe("384px");
    fireEvent.click(screen.getByRole("button", { name: "Dismiss ended pane" }));
    expect(actions.on_dismiss).toHaveBeenCalledOnce();
    expect(mocks.request).not.toHaveBeenCalled();
  });

  it("uses server cell rectangles and keeps pane controls outside the canvas", async () => {
    mocks.request.mockResolvedValue(split);
    render(<SessionViewSurface {...props()} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [left, right] = screen.getAllByLabelText("Terminal input").map((input) => input.closest<HTMLElement>(".view-pane")!);
    expect(left.style.height).toBe("384px");
    expect(right.style.height).toBe(left.style.height);
    expect(left.style.width).toBe("320px");
    expect(right.style.left).toBe("328px");
    expect(right.style.width).toBe("312px");
    expect(left.querySelector("button")).toBeNull();
    expect(right.querySelector("button")).toBeNull();
    expect(screen.getByRole("button", { name: "Split right" }).closest(".view-viewport")).toBeNull();
    await waitFor(() => expect(mocks.connect).toHaveBeenCalledWith(expect.objectContaining({ terminal_id: "b" }), { resize_with_window: false, terminal_id: "b" }));
  });

  it.each([
    ["pane.split_right", "horizontal"],
    ["pane.split_below", "vertical"],
  ] as const)("exposes %s to the palette and targets the focused pane", async (id, axis) => {
    mocks.request.mockResolvedValue(split);
    let commands: AppCommand[] = [];
    const register = (next: AppCommand[]) => { commands = next; };
    const actions = props();
    const mounted = render(<SessionViewSurface {...actions} on_pane_commands={register} />);
    await waitFor(() => expect(commands.find((command) => command.id === id)?.enabled).toBe(true));
    expect(searchCommands(commands, "split")).toHaveLength(2);
    const second = screen.getAllByLabelText("Terminal input")[1];
    act(() => second.focus());
    // Opening the palette moves DOM focus away; retain the selected pane.
    act(() => second.blur());
    await act(async () => { await commands.find((command) => command.id === id)!.run(); });
    expect(mocks.request).toHaveBeenLastCalledWith(session.target, expect.objectContaining({ kind: "split", terminal_id: "b", axis }));
    mounted.rerender(<SessionViewSurface {...actions} phase="disconnected" on_pane_commands={register} />);
    await waitFor(() => expect(commands.every((command) => !command.enabled)).toBe(true));
    mounted.unmount();
    expect(commands).toEqual([]);
  });

  it("keeps a failed split visible across background refreshes until retry", async () => {
    vi.useFakeTimers();
    mocks.request.mockResolvedValueOnce(initial)
      .mockRejectedValueOnce(new Error("Could not spawn shell"))
      .mockResolvedValueOnce(initial)
      .mockResolvedValueOnce(split);
    await act(async () => { render(<SessionViewSurface {...props()} />); });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Split right" })); });
    expect(screen.getByRole("alert").textContent).toBe("Could not spawn shell");
    await act(async () => { vi.advanceTimersByTime(2000); });
    expect(screen.getByRole("alert").textContent).toBe("Could not spawn shell");
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Split right" })); });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2);
  });

  it("splits without remounting the existing terminal", async () => {
    mocks.request.mockResolvedValueOnce(initial).mockResolvedValueOnce(split);
    render(<SessionViewSurface {...props()} />);
    await waitFor(() => expect(mocks.request).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "Split right" }));
    await waitFor(() => {
      expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2);
      expect(mocks.mount).toHaveBeenCalledTimes(2);
    });
    expect(mocks.unmount).not.toHaveBeenCalled();
    expect(mocks.request).toHaveBeenLastCalledWith(session.target, expect.objectContaining({ kind: "split", terminal_id: "a", axis: "horizontal" }));
  });

  it("remembers the new root after promoting a pane", async () => {
    const promoted: SessionView = { ...initial, session_id: "new-root", session_name: "New root", view_id: "new-view", layout: { kind: "terminal", terminal_id: "b" }, panes: [{ terminal_id: "b", left: 0, top: 0, columns: 80, rows: 24 }], terminals: [terminal("b")] };
    mocks.request.mockResolvedValueOnce(split).mockResolvedValueOnce(promoted).mockResolvedValueOnce(initial);
    const actions = props();
    render(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    act(() => screen.getAllByLabelText("Terminal input")[1].focus());
    fireEvent.click(screen.getByRole("button", { name: "Move to new session" }));
    await waitFor(() => expect(actions.on_promoted).toHaveBeenCalledWith(expect.objectContaining({ session_id: "new-root", terminal_id: "b", name: "New root" })));
  });

  it.each([
    ["Split right", "horizontal"],
    ["Split below", "vertical"],
  ] as const)("reveals the new pane when clicking %s from a zoomed pane", async (label, axis) => {
    const expanded: SessionView = {
      ...split, revision: "2", panes: [split.panes[0], { terminal_id: "b", left: 41, top: 0, columns: 19, rows: 24 }, { terminal_id: "c", left: 61, top: 0, columns: 19, rows: 24 }],
      layout: { kind: "split", axis: "horizontal", children: [
        { kind: "terminal", terminal_id: "a" },
        { kind: "split", axis, children: [{ kind: "terminal", terminal_id: "b" }, { kind: "terminal", terminal_id: "c" }] },
      ] },
      terminals: [...split.terminals, terminal("c")],
    };
    mocks.request.mockResolvedValueOnce(split).mockResolvedValue(expanded);
    render(<SessionViewSurface {...props()} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "other" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    act(() => second.focus());
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "z", code: "KeyZ" });
    await waitFor(() => expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden"));
    const split_button = screen.getByRole<HTMLButtonElement>("button", { name: label });
    // The shared view can render before this owner's command settles.
    await waitFor(() => expect(split_button.disabled).toBe(false));
    fireEvent.click(split_button);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(3));
    expect(mocks.request).toHaveBeenLastCalledWith(session.target, expect.objectContaining({ kind: "split", terminal_id: "b", axis }));
    for (const input of screen.getAllByLabelText("Terminal input")) {
      expect(input.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("visible");
    }
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

  it("retains the ended pane until dismissed, then transfers the survivor", async () => {
    vi.useFakeTimers();
    mocks.request.mockResolvedValueOnce(split).mockResolvedValue({ ...initial, layout: { kind: "terminal", terminal_id: "b" }, panes: [{ terminal_id: "b", left: 0, top: 0, columns: 80, rows: 24 }], terminals: [terminal("b")] });
    let finish_detach!: () => void;
    mocks.detach.mockImplementationOnce(() => new Promise<void>((resolve) => { finish_detach = resolve; }));
    const actions = props();
    let mounted!: ReturnType<typeof render>;
    await act(async () => { mounted = render(<SessionViewSurface {...actions} />); });
    expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2);
    await act(async () => { vi.advanceTimersByTime(2000); });
    expect(mocks.detach).not.toHaveBeenCalled();
    expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "Dismiss ended pane" })).toBeNull();
    mounted.rerender(<SessionViewSurface {...actions} phase="ended" />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Dismiss ended pane" })); });
    expect(mocks.detach).toHaveBeenCalled();
    expect(actions.on_select_terminal).not.toHaveBeenCalled();
    await act(async () => { finish_detach(); });
    expect(actions.on_select_terminal).toHaveBeenCalledWith(expect.objectContaining({ terminal_id: "b" }));
  });

  it("waits for dismissal before opening the surviving terminal after exit", async () => {
    mocks.request.mockResolvedValueOnce({ ...initial, layout: { kind: "terminal", terminal_id: "b" }, panes: [{ terminal_id: "b", left: 0, top: 0, columns: 80, rows: 24 }], terminals: [terminal("b")] });
    const actions = props();
    render(<SessionViewSurface {...actions} phase="ended" />);
    expect(actions.on_select_terminal).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss ended pane" }));
    await waitFor(() => expect(actions.on_select_terminal).toHaveBeenCalledWith(expect.objectContaining({ session_id: "root", terminal_id: "b" })));
  });
  it("routes prefix input and split commands to the focused pane without remounting", async () => {
    mocks.request.mockResolvedValue(split);
    const actions = props();
    render(<SessionViewSurface {...actions} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "macos" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    act(() => first.focus());
    fireEvent.keyDown(first, { key: "b", code: "KeyB", ctrlKey: true });
    expect(screen.getByText("Ctrl+B", { selector: "strong" })).toBeTruthy();
    fireEvent.keyDown(first, { key: "ArrowRight", code: "ArrowRight" });
    await waitFor(() => expect(document.activeElement).toBe(second));
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    expect(mocks.input).toHaveBeenCalledExactlyOnceWith(new Uint8Array([2]));
    expect(actions.onInput).not.toHaveBeenCalled();
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "v", code: "KeyV" });
    await waitFor(() => expect(mocks.request).toHaveBeenLastCalledWith(session.target, expect.objectContaining({ kind: "split", terminal_id: "b" })));
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

  it("zooms through the resize owner and moves panes through revision-checked layouts without remounting", async () => {
    mocks.request.mockResolvedValue(split);
    render(<SessionViewSurface {...props()} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "other" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    act(() => first.focus());
    const prefix = () => fireEvent.keyDown(first, { key: "b", code: "KeyB", ctrlKey: true });
    prefix(); fireEvent.keyDown(first, { key: "z", code: "KeyZ" });
    await waitFor(() => expect(second.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden"));
    expect(mocks.zoom).toHaveBeenLastCalledWith("a");
    expect(first.closest<HTMLElement>(".view-pane")?.style.width).toBe("640px");
    expect(first.closest<HTMLElement>(".view-pane")?.style.height).toBe("384px");
    prefix(); fireEvent.keyDown(first, { key: "z", code: "KeyZ" });
    await waitFor(() => expect(second.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("visible"));
    expect(mocks.zoom).toHaveBeenLastCalledWith(null);
    expect(first.closest<HTMLElement>(".view-pane")?.style.width).toBe("320px");
    prefix(); fireEvent.keyDown(first, { key: "m", code: "KeyM" });
    fireEvent.keyDown(first, { key: "ArrowRight", code: "ArrowRight" });
    await waitFor(() => expect(mocks.request).toHaveBeenLastCalledWith(session.target, {
      kind: "update", session_id: "root", expected_revision: "3",
      layout: { kind: "split", axis: "horizontal", children: [{ kind: "terminal", terminal_id: "b" }, { kind: "terminal", terminal_id: "a" }] },
    }));
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

  it("renders shared zoom at full canvas size, selects the zoomed pane, and keeps hidden terminals mounted", async () => {
    mocks.request.mockResolvedValue({ ...split, zoomed_terminal_id: "b" });
    const actions = props();
    render(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    await waitFor(() => expect(document.activeElement).toBe(second));
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden");
    expect(second.closest<HTMLElement>(".view-pane")?.style).toMatchObject({ left: "0px", top: "0px", width: "640px", height: "384px", visibility: "visible" });
    mocks.mounted_inputs[0](new Uint8Array([97]));
    mocks.mounted_inputs[1](new Uint8Array([98]));
    expect(actions.onInput).not.toHaveBeenCalled();
    expect(mocks.input).toHaveBeenCalledExactlyOnceWith(new Uint8Array([98]));
    act(() => publishSessionView({ session, attachment_id: "primary-owner", view: split }));
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("visible");
    expect(second.closest<HTMLElement>(".view-pane")?.style).toMatchObject({ left: "328px", width: "312px" });
    expect(mocks.mount).toHaveBeenCalledTimes(2);
    expect(mocks.unmount).not.toHaveBeenCalled();
    expect(mocks.detach).not.toHaveBeenCalled();
  });

  it("waits for shared unzoom before changing focus", async () => {
    mocks.request.mockResolvedValue({ ...split, zoomed_terminal_id: "b" });
    mocks.zoom.mockResolvedValue(undefined);
    render(<SessionViewSurface {...props()} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "other" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    await waitFor(() => expect(document.activeElement).toBe(second));
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "ArrowLeft", code: "ArrowLeft" });
    expect(mocks.zoom).toHaveBeenCalledExactlyOnceWith(null);
    expect(document.activeElement).toBe(second);
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden");
    act(() => publishSessionView({ session, attachment_id: "primary-owner", view: { ...split, revision: "9" } }));
    await waitFor(() => expect(document.activeElement).toBe(first));
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("visible");
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

  it("shows unsupported-server failures without applying local zoom", async () => {
    mocks.request.mockResolvedValue(split);
    mocks.zoom.mockRejectedValue(new Error("This server does not support pane zoom. Upgrade ctmuxd to use it."));
    render(<SessionViewSurface {...props()} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "other" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    act(() => first.focus());
    fireEvent.keyDown(first, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(first, { key: "z", code: "KeyZ" });
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("does not support pane zoom"));
    expect(second.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("visible");
    expect(first.closest<HTMLElement>(".view-pane")?.style.width).toBe("320px");
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

  it("ignores older view broadcasts from another pane attachment", async () => {
    mocks.request.mockResolvedValue(split);
    render(<SessionViewSurface {...props()} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    act(() => {
      publishSessionView({ session, attachment_id: "primary-owner", view: { ...split, revision: "9007199254740994", zoomed_terminal_id: "b" } });
      publishSessionView({ session, attachment_id: "secondary", view: { ...split, revision: "9007199254740993" } });
    });
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden");
    expect(second.closest<HTMLElement>(".view-pane")?.style.width).toBe("640px");
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

  it("keeps shared zoom and focus when resize ownership is unavailable", async () => {
    stop_control();
    mocks.request.mockResolvedValue({ ...split, zoomed_terminal_id: "b" });
    render(<SessionViewSurface {...props()} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "other" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    await waitFor(() => expect(document.activeElement).toBe(second));
    fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
    fireEvent.keyDown(second, { key: "ArrowLeft", code: "ArrowLeft" });
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Take resize control"));
    expect(mocks.zoom).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(second);
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden");
  });

  it("keeps a newer shared zoom that overtakes the owner's unzoom acknowledgement", async () => {
    mocks.request.mockResolvedValue({ ...split, zoomed_terminal_id: "b" });
    mocks.zoom.mockImplementation(async () => {
      publishSessionView({ session, attachment_id: "primary-owner", view: { ...split, revision: "9" } });
      publishSessionView({ session, attachment_id: "secondary", view: { ...split, revision: "10", zoomed_terminal_id: "b" } });
    });
    render(<SessionViewSurface {...props()} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "other" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    await waitFor(() => expect(document.activeElement).toBe(second));
    await act(async () => {
      fireEvent.keyDown(second, { key: "b", code: "KeyB", ctrlKey: true });
      fireEvent.keyDown(second, { key: "ArrowLeft", code: "ArrowLeft" });
    });
    expect(document.activeElement).toBe(second);
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden");
    expect(second.closest<HTMLElement>(".view-pane")?.style.width).toBe("640px");
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

});
