// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { resolvePrefix } from "./prefixKeymap";
import { useTerminalPrefix } from "./useTerminalPrefix";
afterEach(() => { cleanup(); vi.useRealTimers(); });
const action = vi.fn();
const input = vi.fn();
function Fixture({ enabled = true, context = "one", prefix = "Ctrl+B", repeat_context = context }: { enabled?: boolean; context?: string; prefix?: string; repeat_context?: string }) {
  const state = useTerminalPrefix({ enabled, context, repeat_context, keymap: resolvePrefix({ schema_version: 1, overrides: [], prefix: { key: prefix, bindings: [] } }, new Map(), "macos"), on_action: action, on_input: input });
  return <div onKeyDownCapture={state.onKeyDown}><div className="terminal-container"><textarea aria-label="terminal" /></div><input aria-label="dialog" /><output>{state.mode ?? "idle"}</output></div>;
}
function key(key: string, code = key, ctrlKey = false, target = screen.getByLabelText("terminal")) {
  return fireEvent.keyDown(target, { key, code, ctrlKey });
}
it("captures prefix/actions, cancels with Escape, and sends double prefix exactly once", () => {
  action.mockClear(); input.mockClear(); render(<Fixture />);
  expect(key("a", "KeyA")).toBe(true);
  expect(key("b", "KeyB", true)).toBe(false);
  expect(screen.getByText("prefix")).toBeTruthy();
  key("v", "KeyV"); expect(action).toHaveBeenLastCalledWith("pane.split_right");
  key("b", "KeyB", true); key("Escape"); expect(screen.getByText("idle")).toBeTruthy();
  key("b", "KeyB", true); key("b", "KeyB", true);
  expect(input).toHaveBeenCalledExactlyOnceWith(new Uint8Array([2]));
  expect(action).toHaveBeenCalledTimes(1);
});
it("keeps move mode until Escape and cancels on context changes", () => {
  action.mockClear(); const view = render(<Fixture />);
  key("b", "KeyB", true); key("m", "KeyM"); key("ArrowRight"); key("ArrowDown");
  expect(action.mock.calls).toEqual([["pane.move_right"], ["pane.move_down"]]);
  expect(screen.getByText("move")).toBeTruthy();
  view.rerender(<Fixture context="two" />); expect(screen.getByText("idle")).toBeTruthy();
});
it("does not capture dialogs or disabled terminals and cancels when settings change", () => {
  const view = render(<Fixture />);
  expect(key("b", "KeyB", true, screen.getByLabelText("dialog"))).toBe(true);
  key("b", "KeyB", true);
  view.rerender(<Fixture prefix="Ctrl+A" />); expect(screen.getByText("idle")).toBeTruthy();
  expect(key("b", "KeyB", true)).toBe(true);
  view.rerender(<Fixture enabled={false} />);
  expect(key("b", "KeyB", true)).toBe(true);
});

it("resizes with modified arrows, repeats for 500ms, and leaves later arrows to the terminal", () => {
  vi.useFakeTimers(); action.mockClear(); render(<Fixture />);
  const terminal = screen.getByLabelText("terminal");
  expect(fireEvent.keyDown(terminal, { key: "ArrowLeft", ctrlKey: true })).toBe(true);
  key("b", "KeyB", true);
  fireEvent.keyDown(terminal, { key: "ArrowLeft", ctrlKey: true });
  expect(action).toHaveBeenLastCalledWith("pane.resize_left");
  vi.advanceTimersByTime(400);
  key("Alt", "AltLeft");
  expect(fireEvent.keyDown(terminal, { key: "ArrowDown", altKey: true, repeat: true })).toBe(false);
  expect(action).toHaveBeenLastCalledWith("pane.resize_down_large");
  vi.advanceTimersByTime(501);
  expect(fireEvent.keyDown(terminal, { key: "ArrowLeft", ctrlKey: true, repeat: true })).toBe(true);
  expect(action).toHaveBeenCalledTimes(2);
  key("b", "KeyB", true);
  fireEvent.keyDown(terminal, { key: "ArrowRight", ctrlKey: true });
  expect(key("x", "KeyX")).toBe(true);
  expect(fireEvent.keyDown(terminal, { key: "ArrowRight", ctrlKey: true })).toBe(true);
});
it("cancels resize repeats on context changes, blur, and Escape", () => {
  action.mockClear(); const mounted = render(<Fixture />);
  key("b", "KeyB", true); fireEvent.keyDown(screen.getByLabelText("terminal"), { key: "ArrowUp", ctrlKey: true });
  mounted.rerender(<Fixture context="two" />);
  expect(fireEvent.keyDown(screen.getByLabelText("terminal"), { key: "ArrowUp", ctrlKey: true })).toBe(true);
  key("b", "KeyB", true); fireEvent.keyDown(screen.getByLabelText("terminal"), { key: "ArrowUp", ctrlKey: true });
  fireEvent.blur(window);
  expect(fireEvent.keyDown(screen.getByLabelText("terminal"), { key: "ArrowUp", ctrlKey: true })).toBe(true);
  key("b", "KeyB", true); key("Escape");
  expect(fireEvent.keyDown(screen.getByLabelText("terminal"), { key: "ArrowUp", ctrlKey: true })).toBe(true);
});

it("repeats focus arrows across panes in one session, expires, and cancels for composition", () => {
  vi.useFakeTimers(); action.mockClear(); const mounted = render(<Fixture context="session:a" repeat_context="session" />);
  key("b", "KeyB", true); key("ArrowRight");
  expect(action).toHaveBeenLastCalledWith("pane.focus_right");
  mounted.rerender(<Fixture context="session:b" repeat_context="session" />);
  expect(fireEvent.keyDown(screen.getByLabelText("terminal"), { key: "ArrowDown", repeat: true })).toBe(false);
  expect(action).toHaveBeenLastCalledWith("pane.focus_down");
  vi.advanceTimersByTime(501);
  expect(key("ArrowLeft")).toBe(true);
  key("b", "KeyB", true); key("ArrowRight");
  fireEvent.keyDown(screen.getByLabelText("terminal"), { key: "Process", isComposing: true });
  expect(key("ArrowRight")).toBe(true);
  key("b", "KeyB", true); key("ArrowRight");
  mounted.rerender(<Fixture context="other:a" repeat_context="other" />);
  expect(key("ArrowRight")).toBe(true);
});

it("does not alias Shift arrows to focus actions in prefix or repeat mode", () => {
  action.mockClear(); render(<Fixture />);
  const terminal = screen.getByLabelText("terminal");
  key("b", "KeyB", true);
  fireEvent.keyDown(terminal, { key: "ArrowLeft", shiftKey: true });
  expect(action).not.toHaveBeenCalled();
  key("b", "KeyB", true); key("ArrowRight");
  expect(fireEvent.keyDown(terminal, { key: "ArrowLeft", shiftKey: true, repeat: true })).toBe(true);
  expect(action).toHaveBeenCalledExactlyOnceWith("pane.focus_right");
  expect(key("ArrowLeft")).toBe(true);
});

it("preserves repeat while browser modifier keydowns switch focus and resize strokes", () => {
  vi.useFakeTimers(); action.mockClear(); render(<Fixture />);
  const terminal = screen.getByLabelText("terminal");
  key("b", "KeyB", true);
  fireEvent.keyDown(terminal, { key: "Control", code: "ControlLeft", ctrlKey: true });
  fireEvent.keyDown(terminal, { key: "ArrowRight", ctrlKey: true });
  vi.advanceTimersByTime(400);
  fireEvent.keyDown(terminal, { key: "Alt", code: "AltLeft", altKey: true });
  fireEvent.keyDown(terminal, { key: "ArrowRight", altKey: true });
  expect(action.mock.calls).toEqual([["pane.resize_right"], ["pane.resize_right_large"]]);

  key("b", "KeyB", true); key("ArrowLeft");
  vi.advanceTimersByTime(400);
  fireEvent.keyDown(terminal, { key: "Control", code: "ControlLeft", ctrlKey: true });
  fireEvent.keyDown(terminal, { key: "ArrowDown", ctrlKey: true });
  expect(action.mock.calls.slice(-2)).toEqual([["pane.focus_left"], ["pane.resize_down"]]);
  expect(fireEvent.keyDown(terminal, { key: "ArrowUp", ctrlKey: true, altKey: true })).toBe(true);
  expect(fireEvent.keyDown(terminal, { key: "ArrowDown", ctrlKey: true })).toBe(true);
  expect(action).toHaveBeenCalledTimes(4);
});
