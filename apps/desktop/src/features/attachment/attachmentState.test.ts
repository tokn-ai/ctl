import { describe, expect, it } from "vitest";
import type { SessionSummary } from "../../lib/types";
import { initialAttachmentState, transitionAttachment } from "./attachmentState";

const session: SessionSummary = {
  target: { kind: "local" }, session_id: "session", name: "shell", status: "running",
  next_sequence: "0", terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
};
const opened = {
  ...initialAttachmentState(), phase: "attached" as const, session, attachment_id: "actor",
  input_lease: { held: true, owned_by_client: true }, layout_lease: { held: true, owned_by_client: true },
};

describe("attachment lifecycle", () => {
  it.each([null, "0", "123"])("derives connection intent independently of cursor %s", (resume_from) => {
    for (const intent of ["attach", "reconnect"] as const) {
      const next = transitionAttachment(initialAttachmentState(), { type: "begin", intent, session, resume_from, resize_with_window: false });
      expect(next.phase).toBe(intent === "attach" ? "connecting" : "reconnecting");
      expect(next.reconnect_sequence).toBe(resume_from);
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
