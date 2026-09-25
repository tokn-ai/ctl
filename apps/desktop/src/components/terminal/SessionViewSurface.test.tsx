// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary, SessionView } from "../../lib/types";
import { SessionViewSurface } from "./SessionViewSurface";
import type { AppCommand } from "../../features/commands/types";
import { searchCommands } from "../../features/commands/commandSearch";
import type { XtermRenderer } from "../../features/terminal/XtermRenderer";

const mocks = vi.hoisted(() => ({ request: vi.fn(), mount: vi.fn(), unmount: vi.fn(), connect: vi.fn(), detach: vi.fn(), viewOffline: vi.fn(), input: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ sessionView: mocks.request }));
vi.mock("../../features/attachment/useAttachment", () => ({ useAttachment: () => ({
  state: { phase: "attached", applied_sequence: "0", input_lease: { owned_by_client: true } },
  connect: mocks.connect, detach: mocks.detach, viewOffline: mocks.viewOffline, handleInput: mocks.input, toggleInputLease: vi.fn(), toggleResizeWithWindow: vi.fn(),
}) }));
vi.mock("./TerminalSurface", async () => {
  const { useEffect } = await import("react");
  const renderer = {};
  return { TerminalSurface: ({ onReady, ended_message, on_dismiss }: { onReady(renderer: unknown): void; ended_message?: string | null; on_dismiss?(): void }) => {
    useEffect(() => {
      mocks.mount();
      onReady(renderer);
      return () => { mocks.unmount(); onReady(null); };
    }, []);
    return <div data-testid="terminal-surface" className="terminal-container"><textarea aria-label="Terminal input" />{ended_message && <button onClick={on_dismiss}>Dismiss ended pane</button>}</div>;
  } };
});

const session: SessionSummary = { target: { kind: "local" }, session_id: "root", view_id: "view", terminal_id: "a", name: "Root", status: "running", next_sequence: "0", terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null } };
const terminal = (terminal_id: string) => ({ terminal_id, name: terminal_id, terminal_size: session.terminal_size, next_sequence: "0" });
const initial: SessionView = { session_id: "root", session_name: "Root", view_id: "view", revision: "0", canvas_size: session.terminal_size, panes: [{ terminal_id: "a", left: 0, top: 0, columns: 80, rows: 24 }], layout: { kind: "terminal", terminal_id: "a" }, terminals: [terminal("a")] };
const split: SessionView = { ...initial, revision: "1", panes: [{ terminal_id: "a", left: 0, top: 0, columns: 40, rows: 24 }, { terminal_id: "b", left: 41, top: 0, columns: 39, rows: 24 }], layout: { kind: "split", axis: "horizontal", children: [{ kind: "terminal", terminal_id: "a" }, { kind: "terminal", terminal_id: "b" }] }, terminals: [terminal("a"), terminal("b")] };
const props = () => ({ session, on_promoted: vi.fn(), on_select_terminal: vi.fn(), phase: "attached" as const, hasSession: true, has_cached_content: true, onInput: vi.fn(), onReady: vi.fn() });

afterEach(() => { cleanup(); localStorage.clear(); vi.clearAllMocks(); vi.useRealTimers(); });

describe("session compositor", () => {
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

  it("restores the saved split layout after the compositor is remounted offline", async () => {
    mocks.request.mockResolvedValue(split);
    const actions = props();
    const live = render(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    live.unmount();
    mocks.request.mockClear();
    mocks.connect.mockClear();
    render(<SessionViewSurface {...actions} offline phase="disconnected" />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    expect(mocks.request).not.toHaveBeenCalled();
    expect(mocks.connect).not.toHaveBeenCalled();
    expect(mocks.viewOffline).toHaveBeenCalledWith(expect.objectContaining({ terminal_id: "b" }));
  });

  it("keeps split panes locally while a host is paused and marks the active border disconnected", async () => {
    mocks.request.mockResolvedValue(split);
    const actions = props();
    const mounted = render(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    mocks.connect.mockClear();
    mocks.request.mockClear();
    mounted.rerender(<SessionViewSurface {...actions} offline phase="disconnected" />);
    await waitFor(() => expect(mocks.viewOffline).toHaveBeenCalledWith(expect.objectContaining({ terminal_id: "b" })));
    expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2);
    const active = mounted.container.querySelector('[data-active="true"]');
    expect(active?.getAttribute("data-disconnected")).toBe("true");
    expect(screen.getByText("Disconnected · cached view")).toBeTruthy();
    expect(mocks.request).not.toHaveBeenCalled();
    expect(mocks.connect).not.toHaveBeenCalled();
    mounted.rerender(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(mocks.connect).toHaveBeenCalledOnce());
    expect(active?.getAttribute("data-disconnected")).toBe("false");
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
    expect(first.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden");
    fireEvent.click(screen.getByRole("button", { name: label }));
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
    await act(async () => { render(<SessionViewSurface {...actions} />); });
    expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2);
    await act(async () => { vi.advanceTimersByTime(2000); });
    expect(mocks.detach).not.toHaveBeenCalled();
    expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2);
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

  it("zooms and moves panes through revision-checked layouts without remounting", async () => {
    mocks.request.mockResolvedValue(split);
    render(<SessionViewSurface {...props()} prefix_settings={{ document: { schema_version: 1, overrides: [] }, bindings: new Map(), platform: "other" }} />);
    await waitFor(() => expect(screen.getAllByLabelText("Terminal input")).toHaveLength(2));
    const [first, second] = screen.getAllByLabelText("Terminal input");
    act(() => first.focus());
    const prefix = () => fireEvent.keyDown(first, { key: "b", code: "KeyB", ctrlKey: true });
    prefix(); fireEvent.keyDown(first, { key: "z", code: "KeyZ" });
    expect(second.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("hidden");
    prefix(); fireEvent.keyDown(first, { key: "z", code: "KeyZ" });
    expect(second.closest<HTMLElement>(".view-pane")?.style.visibility).toBe("visible");
    prefix(); fireEvent.keyDown(first, { key: "m", code: "KeyM" });
    fireEvent.keyDown(first, { key: "ArrowRight", code: "ArrowRight" });
    await waitFor(() => expect(mocks.request).toHaveBeenLastCalledWith(session.target, {
      kind: "update", session_id: "root", expected_revision: "1",
      layout: { kind: "split", axis: "horizontal", children: [{ kind: "terminal", terminal_id: "b" }, { kind: "terminal", terminal_id: "a" }] },
    }));
    expect(mocks.unmount).not.toHaveBeenCalled();
  });

});
