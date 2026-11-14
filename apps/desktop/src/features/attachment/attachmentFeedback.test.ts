import { afterEach, describe, expect, it, vi } from "vitest";
import type { AttachmentViewState, SessionSummary } from "../../lib/types";
import { formatText, type Locale } from "../i18n/messages";
import { attachmentFeedback } from "./attachmentFeedback";

const local_session: SessionSummary = {
  target: { kind: "local" }, session_id: "shell", name: "Shell", status: "running", next_sequence: "0",
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
};
const remote_session: SessionSummary = {
  ...local_session, target: { kind: "ssh", host_id: "remote", destination: "example", remote_platform: "unix" },
};
type FeedbackState = Pick<AttachmentViewState, "session" | "phase" | "error_code" | "message">;
function state(overrides: Partial<FeedbackState> = {}): FeedbackState {
  return { session: remote_session, phase: "error", error_code: "transport_error", message: "Connection interrupted.", ...overrides };
}

afterEach(() => vi.unstubAllGlobals());

function displayedFeedback(feedback_state: FeedbackState, windows_client?: boolean, locale: Locale = "en") {
  const feedback = attachmentFeedback(feedback_state, windows_client);
  return {
    message: feedback.message === null ? null : formatText(locale, feedback.message),
    action: feedback.action && { ...feedback.action, label: formatText(locale, feedback.action.label) },
  };
}

describe("attachment feedback recovery capabilities", () => {
  it.each(["error", "disconnected", "retry_wait"] as const)("offers reconnect for an ordinary %s state", (phase) => {
    expect(displayedFeedback(state({ phase }), false)).toEqual({
      message: "Connection interrupted.", action: { kind: "reconnect", label: "Reconnect" },
    });
  });

  it.each(["idle", "connecting", "reconnecting", "attached", "ended"] as const)("does not offer recovery during %s", (phase) => {
    expect(displayedFeedback(state({ phase, error_code: "protocol_version_mismatch" }), false)).toEqual({
      message: "Connection interrupted.", action: null,
    });
  });

  it("does not offer an action without a session", () => {
    expect(displayedFeedback(state({ session: null }), false).action).toBeNull();
  });

  it.each([false, true])("preserves local daemon recovery on a Windows client=%s", (windows_client) => {
    expect(displayedFeedback(state({ session: local_session, error_code: "protocol_version_mismatch" }), windows_client)).toEqual({
      message: "Connection interrupted.", action: { kind: "recover_components", label: "Restart local daemon" },
    });
  });

  it("offers remote component updates only where automated maintenance is supported", () => {
    expect(displayedFeedback(state({ error_code: "protocol_version_mismatch" }), false)).toEqual({
      message: "Connection interrupted.", action: { kind: "recover_components", label: "Update remote components" },
    });
  });

  it.each([
    { windows_client: false, remote_platform: "windows" as const, guidance: "Update the components on the Windows host, then reconnect." },
    { windows_client: true, remote_platform: "windows" as const, guidance: "Update the components on the Windows host, then reconnect." },
    { windows_client: true, remote_platform: "unix" as const, guidance: "Update the remote components on the host, then reconnect." },
  ])("requires manual remote maintenance for Windows client=$windows_client and $remote_platform host", ({ windows_client, remote_platform, guidance }) => {
    const session = { ...remote_session, target: { kind: "ssh" as const, host_id: "remote", destination: "example", remote_platform } };
    expect(displayedFeedback(state({ session, error_code: "protocol_version_mismatch" }), windows_client)).toEqual({
      message: `Connection interrupted. ${guidance}`, action: { kind: "connect_host", label: "Connect host" },
    });
  });

  it("retains the Windows setup instructions and connects through the host flow", () => {
    const message = "Install the Windows components with ctlup, then reconnect.";
    expect(displayedFeedback(state({ error_code: "windows_remote_agent_setup_required", message }), false)).toEqual({
      message, action: { kind: "connect_host", label: "Connect host" },
    });
  });

  it.each(["protocol_version_mismatch", "windows_remote_agent_setup_required"])("does not replace %s with a bare reconnect outside its error state", (error_code) => {
    for (const phase of ["disconnected", "retry_wait"] as const)
      expect(displayedFeedback(state({ phase, error_code }), false).action).toBeNull();
  });

  it("leaves absent diagnostics absent while retaining the supported recovery action", () => {
    expect(displayedFeedback(state({ message: null, error_code: "protocol_version_mismatch" }), true)).toEqual({
      message: null, action: { kind: "connect_host", label: "Connect host" },
    });
  });

  it("formats the same recovery guidance in the current locale while retaining raw diagnostics", () => {
    const feedback_state = state({ error_code: "protocol_version_mismatch" });
    expect(displayedFeedback(feedback_state, true, "zh-Hans")).toEqual({
      message: "Connection interrupted. 请更新主机上的远程组件，然后重新连接。",
      action: { kind: "connect_host", label: "连接主机" },
    });
  });

  it("detects the Windows client when no capability override is supplied", () => {
    vi.stubGlobal("navigator", { platform: "Win32" });
    expect(displayedFeedback(state({ error_code: "protocol_version_mismatch" })).action).toEqual({ kind: "connect_host", label: "Connect host" });
    vi.stubGlobal("navigator", { platform: "MacIntel" });
    expect(displayedFeedback(state({ error_code: "protocol_version_mismatch" })).action).toEqual({ kind: "recover_components", label: "Update remote components" });
  });
});
