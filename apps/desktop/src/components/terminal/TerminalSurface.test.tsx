// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TerminalSurface } from "./TerminalSurface";
const mocks = vi.hoisted(() => ({ input: null as null | ((data: Uint8Array) => void), dispose: vi.fn() }));
vi.mock("../../features/terminal/XtermRenderer", () => ({ XtermRenderer: class {
  constructor(_container: HTMLElement, input: (data: Uint8Array) => void) { mocks.input = input; }
  dispose = mocks.dispose;
} }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
describe("terminal exit acknowledgement", () => {
  it("retains the renderer, blocks PTY input, and consumes the dismissal key", () => {
    const input = vi.fn(), dismiss = vi.fn(), ready = vi.fn(), parent = vi.fn();
    const props = { hasSession: true, has_cached_content: true, onInput: input, onReady: ready, on_dismiss: dismiss };
    const mounted = render(<div onKeyDown={parent}><TerminalSurface {...props} phase="attached" /></div>);
    mounted.rerender(<div onKeyDown={parent}><TerminalSurface {...props} phase="ended" ended_message="Exited (code 7)" /></div>);
    expect(mocks.dispose).not.toHaveBeenCalled();
    mocks.input?.(new Uint8Array([65]));
    expect(input).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByRole("button", { name: "Close" }), { key: "a" });
    expect(dismiss).toHaveBeenCalledOnce();
    expect(parent).not.toHaveBeenCalled();
    expect(screen.getByRole("status").textContent).toContain("code 7");
  });
  it("does not treat a reconnecting transport as an exited session", () => {
    const dismiss = vi.fn();
    const mounted = render(<TerminalSurface phase="reconnecting" hasSession has_cached_content onInput={vi.fn()} onReady={vi.fn()} on_dismiss={dismiss} />);
    fireEvent.keyDown(mounted.container.firstChild!, { key: "a" });
    expect(dismiss).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Close" })).toBeNull();
  });
});
