// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttachmentViewState, SessionSummary } from "../../lib/types";
import type { AppCommand } from "../../features/commands/types";
import { CommandProvider } from "../../features/commands/CommandContext";
import { CommandDispatcher } from "../../features/commands/CommandDispatcher";
import { COMMAND_IDS } from "../../features/commands/commandIds";
import { SessionBanner, SessionConnectionBanner } from "./SessionBanner";

const local_session: SessionSummary = {
  target: { kind: "local" }, session_id: "local", name: "Local", status: "running", next_sequence: "0",
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
};
const remote_session: SessionSummary = {
  ...local_session, session_id: "remote", name: "Remote",
  target: { kind: "ssh", host_id: "runtime-host", destination: "runtime.example", remote_platform: "unix" },
};
type ConnectionState = Pick<AttachmentViewState, "session" | "phase" | "error_code" | "message">;
function state(overrides: Partial<ConnectionState> = {}): ConnectionState {
  return { session: remote_session, phase: "error", error_code: "transport_error", message: "SSH connection interrupted.", ...overrides };
}
function renderConnection(connection_state: ConnectionState, commands: AppCommand[] = []) {
  const dispatcher = new CommandDispatcher();
  dispatcher.update(commands, true, vi.fn());
  const reconnect = vi.fn();
  const mounted = render(<CommandProvider value={{ dispatcher, keybinding: () => undefined }}>
    <SessionConnectionBanner state={connection_state} on_reconnect={reconnect} reconnect_available interaction_enabled retry_paused={false} />
  </CommandProvider>);
  return { ...mounted, dispatcher, reconnect };
}
beforeEach(() => vi.stubGlobal("navigator", { platform: "MacIntel" }));
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.unstubAllGlobals(); });

describe("session banner recovery", () => {
  it("keeps the diagnostic and reconnect action together without stealing focus", () => {
    const input = document.createElement("input");
    document.body.append(input);
    input.focus();
    const { reconnect } = renderConnection(state());
    const banner = screen.getByRole("status", { name: "Session connection" });
    expect(document.activeElement).toBe(input);
    expect(within(banner).getByText("Disconnected")).toBeTruthy();
    expect(within(banner).getByText("SSH connection interrupted.")).toBeTruthy();
    fireEvent.click(within(banner).getByRole("button", { name: "Reconnect" }));
    expect(reconnect).toHaveBeenCalledOnce();
    input.remove();
  });

  it.each([
    { session: remote_session, platform: "MacIntel", command_id: COMMAND_IDS.updateComponents, label: "Update remote components", target_key: "host:runtime-host" },
    { session: local_session, platform: "Win32", command_id: COMMAND_IDS.restartDaemon, label: "Restart local daemon", target_key: "local" },
    { session: { ...remote_session, target: { ...remote_session.target, remote_platform: "windows" as const } }, platform: "MacIntel", command_id: COMMAND_IDS.connectHost, label: "Connect host", target_key: "host:runtime-host" },
    { session: remote_session, platform: "Win32", command_id: COMMAND_IDS.connectHost, label: "Connect host", target_key: "host:runtime-host" },
  ])("routes $label through the command dispatcher for the exact session target", ({ session, platform, command_id, label, target_key }) => {
    vi.stubGlobal("navigator", { platform });
    const run = vi.fn();
    const { reconnect } = renderConnection(state({ session, error_code: "protocol_version_mismatch", message: "Incompatible daemon." }), [{
      id: command_id, category: "Session", title: label, enabled: false,
      isEnabled: (args) => args.target_key === target_key, run,
    }]);
    const banner = screen.getByRole("status", { name: "Session connection" });
    expect(within(banner).queryByRole("button", { name: "Reconnect" })).toBeNull();
    const recovery = within(banner).getByRole("button", { name: label });
    expect(recovery.hasAttribute("disabled")).toBe(false);
    fireEvent.click(recovery);
    expect(run).toHaveBeenCalledExactlyOnceWith({ target_key });
    expect(reconnect).not.toHaveBeenCalled();
  });

  it("preserves Windows setup instructions and avoids a bare attachment reconnect", () => {
    const run = vi.fn();
    const message = "Install Windows components with ctlup, then reconnect.";
    const { reconnect } = renderConnection(state({ error_code: "windows_remote_agent_setup_required", message }), [{
      id: COMMAND_IDS.connectHost, category: "Host", title: "Connect host", enabled: true, run,
    }]);
    const banner = screen.getByRole("status", { name: "Session connection" });
    expect(within(banner).getByText(message)).toBeTruthy();
    expect(within(banner).queryByRole("button", { name: "Reconnect" })).toBeNull();
    fireEvent.click(within(banner).getByRole("button", { name: "Connect host" }));
    expect(run).toHaveBeenCalledExactlyOnceWith({ target_key: "host:runtime-host" });
    expect(reconnect).not.toHaveBeenCalled();
  });

  it.each([
    { error_code: "protocol_version_mismatch", phase: "disconnected" as const },
    { error_code: "protocol_version_mismatch", phase: "retry_wait" as const },
    { error_code: "windows_remote_agent_setup_required", phase: "disconnected" as const },
    { error_code: "windows_remote_agent_setup_required", phase: "retry_wait" as const },
  ])("does not leak a bare reconnect for $error_code during $phase", ({ error_code, phase }) => {
    const message = "Prepare the host components before reconnecting.";
    const { reconnect } = renderConnection(state({ error_code, phase, message }), [
      { id: COMMAND_IDS.connectHost, category: "Host", title: "Connect host", enabled: true, run: vi.fn() },
      { id: COMMAND_IDS.updateComponents, category: "App", title: "Update components", enabled: true, run: vi.fn() },
    ]);
    const banner = screen.getByRole("status", { name: "Session connection" });
    expect(within(banner).getByText(message)).toBeTruthy();
    expect(within(banner).queryByRole("button")).toBeNull();
    expect(reconnect).not.toHaveBeenCalled();
  });

  it("keeps recovery disabled until its command is available and respects an active dialog scope", () => {
    const run = vi.fn();
    const { dispatcher } = renderConnection(state({ error_code: "protocol_version_mismatch" }));
    const recovery = screen.getByRole("button", { name: "Update remote components" });
    expect(recovery.hasAttribute("disabled")).toBe(true);
    act(() => dispatcher.update([{
      id: COMMAND_IDS.updateComponents, category: "App", title: "Update components", enabled: true, run,
    }], true, vi.fn()));
    expect(recovery.hasAttribute("disabled")).toBe(false);
    const scope = Symbol("dialog");
    act(() => dispatcher.setScope(scope, { commands: [] }));
    expect(recovery.hasAttribute("disabled")).toBe(true);
    fireEvent.click(recovery);
    expect(run).not.toHaveBeenCalled();
    act(() => dispatcher.removeScope(scope));
    fireEvent.click(recovery);
    expect(run).toHaveBeenCalledExactlyOnceWith({ target_key: "host:runtime-host" });
  });

  it("retains a non-actionable diagnostic when no recovery is available", () => {
    render(<SessionBanner title="Session unavailable" message="The selected session no longer exists." severity="error" />);
    const banner = screen.getByRole("alert", { name: "Session status" });
    expect(within(banner).getByText("The selected session no longer exists.")).toBeTruthy();
    expect(within(banner).queryByRole("button")).toBeNull();
  });
});
