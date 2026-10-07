// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { initialAttachmentState } from "../../features/attachment/attachmentState";
import { TerminalToolbar } from "./TerminalToolbar";

afterEach(cleanup);
const session = { target: { kind: "local" as const }, session_id: "session", name: "shell", status: "running" as const,
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null }, next_sequence: "0" };
function props() {
  return { state: { ...initialAttachmentState(), phase: "attached" as const, session }, resize_control_status: "available" as const,
    onToggleInput: vi.fn(), onToggleResizeWithWindow: vi.fn(), onRequestResizeControl: vi.fn(), onReconnect: vi.fn(),
    onShowCommands: vi.fn(), commandShortcutLabel: "" };
}

describe("terminal toolbar resize controls", () => {
  it("offers manual resize control independently from fixed canvas size", () => {
    const actions = props();
    render(<TerminalToolbar {...actions} />);
    expect(screen.getByLabelText("Resize control status").textContent).toBe("Resize: Available");
    fireEvent.click(screen.getByRole("button", { name: "Take resize control" }));
    expect(actions.onRequestResizeControl).toHaveBeenCalledExactlyOnceWith(true);
    expect(actions.onToggleResizeWithWindow).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Resize with window" }));
    expect(actions.onToggleResizeWithWindow).toHaveBeenCalledOnce();
  });

  it("shows the aggregate local owner while leaving auto resizing off", () => {
    const actions = props();
    render(<TerminalToolbar {...actions} resize_control_status="owned" />);
    expect(screen.getByLabelText("Resize control status").textContent).toBe("Resize: Owned here");
    expect(screen.getByRole("button", { name: "Resize with window" }).textContent).toContain("Fixed size");
    fireEvent.click(screen.getByRole("button", { name: "Release resize control" }));
    expect(actions.onRequestResizeControl).toHaveBeenCalledExactlyOnceWith(false);
    expect(actions.onToggleResizeWithWindow).not.toHaveBeenCalled();
  });

  it("stops auto resizing with its own control while retaining manual resize ownership", () => {
    const actions = props();
    render(<TerminalToolbar {...actions} resize_control_status="owned" state={{ ...actions.state, resize_with_window: true }} />);
    fireEvent.click(screen.getByRole("button", { name: "Use fixed size" }));
    expect(actions.onToggleResizeWithWindow).toHaveBeenCalledOnce();
    expect(actions.onRequestResizeControl).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Release resize control" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("allows an ordinary acquire to refresh held state while explaining that the current owner must release", () => {
    const actions = props();
    render(<TerminalToolbar {...actions} resize_control_status="held_elsewhere" />);
    expect(screen.getByLabelText("Resize control status").textContent).toBe("Resize: Held elsewhere");
    const take = screen.getByRole("button", { name: "Take resize control" });
    expect(take.hasAttribute("disabled")).toBe(false);
    expect(take.title).toContain("current owner must release");
    fireEvent.click(take);
    expect(actions.onRequestResizeControl).toHaveBeenCalledExactlyOnceWith(true);
  });
});
