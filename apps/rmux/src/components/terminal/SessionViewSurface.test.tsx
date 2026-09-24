// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary, SessionView } from "../../lib/types";
import { SessionViewSurface } from "./SessionViewSurface";
import type { AppCommand } from "../../features/commands/types";
import { searchCommands } from "../../features/commands/commandSearch";

const mocks = vi.hoisted(() => ({ request: vi.fn(), mount: vi.fn(), unmount: vi.fn(), connect: vi.fn(), detach: vi.fn(), input: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ sessionView: mocks.request }));
vi.mock("../../features/attachment/useAttachment", () => ({ useAttachment: () => ({
  state: { phase: "attached", applied_sequence: "0", input_lease: { owned_by_client: true } },
  connect: mocks.connect, detach: mocks.detach, handleInput: mocks.input, toggleInputLease: vi.fn(), toggleResizeWithWindow: vi.fn(),
}) }));
vi.mock("./TerminalSurface", async () => {
  const { useEffect } = await import("react");
  const renderer = {};
  return { TerminalSurface: ({ onReady }: { onReady(renderer: unknown): void }) => {
    useEffect(() => {
      mocks.mount();
      onReady(renderer);
      return () => { mocks.unmount(); onReady(null); };
    }, []);
    return <div data-testid="terminal-surface" className="terminal-container"><textarea aria-label="Terminal input" /></div>;
  } };
});

const session: SessionSummary = { target: { kind: "local" }, session_id: "root", view_id: "view", terminal_id: "a", name: "Root", status: "running", next_sequence: "0", terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null } };
const terminal = (terminal_id: string) => ({ terminal_id, name: terminal_id, terminal_size: session.terminal_size, next_sequence: "0" });
const initial: SessionView = { session_id: "root", session_name: "Root", view_id: "view", revision: "0", layout: { kind: "terminal", terminal_id: "a" }, terminals: [terminal("a")] };
const split: SessionView = { ...initial, revision: "1", layout: { kind: "split", axis: "horizontal", children: [{ kind: "terminal", terminal_id: "a" }, { kind: "terminal", terminal_id: "b" }] }, terminals: [terminal("a"), terminal("b")] };
const props = () => ({ session, available_sessions: [session], on_promoted: vi.fn(), on_merged: vi.fn(), on_select_terminal: vi.fn(), phase: "attached" as const, hasSession: true, has_cached_content: true, onInput: vi.fn(), onReady: vi.fn() });

afterEach(() => { cleanup(); vi.clearAllMocks(); vi.useRealTimers(); });

describe("session compositor", () => {
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
    await waitFor(() => expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2));
    expect(mocks.mount).toHaveBeenCalledTimes(2);
    expect(mocks.unmount).not.toHaveBeenCalled();
    expect(mocks.request).toHaveBeenLastCalledWith(session.target, expect.objectContaining({ kind: "split", terminal_id: "a", axis: "horizontal" }));
  });

  it("remembers the new root after promoting a pane", async () => {
    const promoted: SessionView = { ...initial, session_id: "new-root", session_name: "New root", view_id: "new-view", layout: { kind: "terminal", terminal_id: "b" }, terminals: [terminal("b")] };
    mocks.request.mockResolvedValueOnce(split).mockResolvedValueOnce(promoted).mockResolvedValueOnce(initial);
    const actions = props();
    render(<SessionViewSurface {...actions} />);
    await waitFor(() => expect(screen.getAllByRole("button", { name: "Move to new session" })).toHaveLength(2));
    fireEvent.click(screen.getAllByRole("button", { name: "Move to new session" })[1]);
    await waitFor(() => expect(actions.on_promoted).toHaveBeenCalledWith(expect.objectContaining({ session_id: "new-root", terminal_id: "b", name: "New root" })));
  });

  it.each([
    ["Split right", "horizontal"],
    ["Split below", "vertical"],
  ] as const)("reveals the new pane when clicking %s from a zoomed pane", async (label, axis) => {
    const expanded: SessionView = {
      ...split, revision: "2",
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

  it("releases the surviving pane attachment before transferring it to the primary renderer", async () => {
    vi.useFakeTimers();
    mocks.request.mockResolvedValueOnce(split).mockResolvedValueOnce({ ...initial, layout: { kind: "terminal", terminal_id: "b" }, terminals: [terminal("b")] });
    let finish_detach!: () => void;
    mocks.detach.mockImplementationOnce(() => new Promise<void>((resolve) => { finish_detach = resolve; }));
    const actions = props();
    await act(async () => { render(<SessionViewSurface {...actions} />); });
    expect(screen.getAllByTestId("terminal-surface")).toHaveLength(2);
    await act(async () => { vi.advanceTimersByTime(2000); });
    expect(mocks.detach).toHaveBeenCalled();
    expect(actions.on_select_terminal).not.toHaveBeenCalled();
    await act(async () => { finish_detach(); });
    expect(actions.on_select_terminal).toHaveBeenCalledWith(expect.objectContaining({ terminal_id: "b" }));
  });

  it("opens the surviving terminal when the primary exits", async () => {
    mocks.request.mockResolvedValueOnce({ ...initial, layout: { kind: "terminal", terminal_id: "b" }, terminals: [terminal("b")] });
    const actions = props();
    render(<SessionViewSurface {...actions} phase="ended" />);
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
