import { describe, expect, it } from "vitest";
import type { OpenAttachmentResponse, SessionSummary } from "../../lib/types";
import { initialAttachmentState, transitionAttachment } from "./attachmentState";

const session: SessionSummary = {
  target: { kind: "local" }, session_id: "session", name: "shell", status: "running",
  next_sequence: "0", terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
};
const opened = {
  ...initialAttachmentState(), phase: "attached" as const, session, attachment_id: "actor",
  input_lease: { held: true, owned_by_client: true }, layout_lease: { held: true, owned_by_client: true },
};
const response: OpenAttachmentResponse = {
  attachment_id: "actor", session, replay_from: "0", history_gap: true, terminal_size_mismatch: false,
  input_lease: { held: true, owned_by_client: true }, layout_lease: { held: false, owned_by_client: false },
  shell_state: { shell_type: "unknown", cwd: null, running_command: null, prompt_phase: "unknown", tui_hint: "unknown", revision: "0", observed_sequence: "0" },
};

describe("attachment lifecycle", () => {
  it("begins history loading without claiming a gap", () => {
    expect(initialAttachmentState()).toMatchObject({ history_loading: false, history_gap: false });
    const beginning = transitionAttachment(initialAttachmentState(), { type: "begin", intent: "attach", session,
      resume_from: null, resize_with_window: false });
    expect(beginning).toMatchObject({ history_loading: true, history_gap: false });
  });

  it.each([undefined, false, true])("keeps response raw replay gaps separate from pending history (%s)", (history_loading) => {
    const beginning = transitionAttachment(initialAttachmentState(), { type: "begin", intent: "reconnect", session,
      resume_from: "0", resize_with_window: false });
    const attached = transitionAttachment(beginning, { type: "attached", response, resize_with_window: false, history_loading });
    expect(attached).toMatchObject({ phase: "attached", history_loading: history_loading ?? false, history_gap: false });
  });

  it("preserves known truncation when the same terminal reconnects without a replacing checkpoint", () => {
    const retained = { ...opened, session: { ...session, terminal_id: "pane" }, history_gap: true };
    const beginning = transitionAttachment(retained, { type: "begin", intent: "reconnect", session: retained.session,
      resume_from: "10", resize_with_window: false });
    expect(beginning).toMatchObject({ history_loading: true, history_gap: true });
    const attached = transitionAttachment(beginning, { type: "attached", response: { ...response, session: retained.session, history_gap: false },
      resize_with_window: false, history_loading: false });
    expect(attached).toMatchObject({ history_loading: false, history_gap: true });
  });

  it("recognizes normalized root terminal identity when preserving known truncation", () => {
    const beginning = transitionAttachment({ ...opened, history_gap: true }, { type: "begin", intent: "reconnect",
      session: { ...session, terminal_id: session.session_id }, resume_from: "10", resize_with_window: false });
    expect(beginning.history_gap).toBe(true);
  });

  it.each(["host", "session", "terminal"])("does not copy known truncation into a different %s", (change) => {
    const retained = { ...opened, session: { ...session, terminal_id: "pane" }, history_gap: true };
    const selected: SessionSummary = {
      ...retained.session,
      ...(change === "host" ? { target: { kind: "ssh" as const, destination: "other" } } : {}),
      ...(change === "session" ? { session_id: "other" } : {}),
      ...(change === "terminal" ? { terminal_id: "other" } : {}),
    };
    const beginning = transitionAttachment(retained, { type: "begin", intent: "attach", session: selected,
      resume_from: null, resize_with_window: false });
    expect(beginning.history_gap).toBe(false);
  });

  it.each([false, true])("reports unavailable resize control accurately (held=%s)", (held) => {
    const connecting = transitionAttachment(initialAttachmentState(), { type: "begin", intent: "attach", session, resume_from: null,
      resize_with_window: false, resize_control_desired: true });
    const next = transitionAttachment(connecting, { type: "attached", resize_with_window: false, response: {
      attachment_id: "actor", session, replay_from: "0", history_gap: false, terminal_size_mismatch: false,
      input_lease: { held: false, owned_by_client: false }, layout_lease: { held, owned_by_client: false },
      shell_state: { shell_type: "unknown", cwd: null, running_command: null, prompt_phase: "unknown", tui_hint: "unknown", revision: "0", observed_sequence: "0" },
    } });
    expect(next.message).toBe(held ? "Another attachment holds resize control." : "Resize control is available. Take resize control to resize panes.");
    expect(next.resize_control_desired).toBe(true);
  });
  it.each([null, "0", "123"])("derives connection intent independently of cursor %s", (resume_from) => {
    for (const intent of ["attach", "reconnect"] as const) {
      const next = transitionAttachment(initialAttachmentState(), { type: "begin", intent, session, resume_from, resize_with_window: false });
      expect(next.phase).toBe(intent === "attach" ? "connecting" : "reconnecting");
      expect(next.reconnect_sequence).toBe(resume_from);
      expect(next.history_loading).toBe(true);
    }
  });

  it("never infers process exit from a broken connection or failed retry", () => {
    const disconnected = transitionAttachment(opened, { type: "closed", reason: "connection_closed", next_sequence: null });
    const waiting = transitionAttachment(disconnected, { type: "retry_scheduled", retry_at_ms: 1_000 });
    const stopped = transitionAttachment(waiting, { type: "retry_exhausted" });
    expect(waiting).toMatchObject({ phase: "retry_wait", retry_at_ms: 1_000, attachment_id: null });
    expect(stopped).toMatchObject({ phase: "error", retry_at_ms: null, error_code: "automatic_reconnect_timeout" });
    expect(stopped.session?.status).toBe("running"); // Last observation, not a fresh process claim.
    for (const state of [disconnected, waiting, stopped]) {
      expect(state.input_lease.owned_by_client).toBe(false);
      expect(state.layout_lease.owned_by_client).toBe(false);
    }
  });

  it("preserves confirmed exit through trailing transport failure and close events", () => {
    const ended = transitionAttachment(opened, { type: "ended", exit_code: 7 });
    const failed = transitionAttachment(ended, { type: "failed", code: "backend_error", message: "Pipe closed", resume_from: null });
    const closed = transitionAttachment(failed, { type: "closed", reason: "connection_closed", next_sequence: "42" });
    expect(closed).toMatchObject({ phase: "ended", message: "Session ended with exit code 7.", attachment_id: null });
    expect(transitionAttachment(closed, { type: "retry_scheduled", retry_at_ms: 1_000 })).toEqual(closed);
    expect(transitionAttachment(closed, { type: "retry_exhausted" })).toEqual(closed);
  });

  it("revokes capabilities on a failure while preserving the session for inspection", () => {
    const failed = transitionAttachment(opened, { type: "failed", code: "protocol_version_mismatch", message: "Update required", resume_from: null });
    expect(failed).toMatchObject({ phase: "error", session, attachment_id: null, input_lease: { held: false, owned_by_client: false }, layout_lease: { held: false, owned_by_client: false } });
  });

  it("does not schedule retries for an active attachment or an idle view", () => {
    for (const state of [opened, initialAttachmentState()]) {
      expect(transitionAttachment(state, { type: "retry_scheduled", retry_at_ms: 1_000 })).toBe(state);
    }
  });
});
