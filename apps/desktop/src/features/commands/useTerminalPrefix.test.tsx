// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { resolvePrefix } from "./prefixKeymap";
import { useTerminalPrefix } from "./useTerminalPrefix";
afterEach(cleanup);
const action = vi.fn();
const input = vi.fn();
function Fixture({ enabled = true, context = "one", prefix = "Ctrl+B" }) {
  const state = useTerminalPrefix({ enabled, context, keymap: resolvePrefix({ schema_version: 1, overrides: [], prefix: { key: prefix, bindings: [] } }, new Map(), "macos"), on_action: action, on_input: input });
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
