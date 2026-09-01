import { describe, expect, it } from "vitest";
import type { AttachmentViewState } from "../../lib/types";
import {
  ATTACHMENT_RECOVERY_WINDOW_MS,
  ATTACHMENT_RECOVERY_STABILITY_MS,
  AttachmentRecoveryBackoff,
  canAutomaticallyRecoverAttachment,
  interruptedAttachmentState,
  reconnectSequenceAfterError,
} from "./attachmentRecovery";

function attachedState(
  overrides: Partial<AttachmentViewState> = {},
): AttachmentViewState {
  return {
    phase: "attached",
    error_code: null,
    attachment_id: "attachment-1",
    session: {
      session_id: "session-1",
      name: "work",
      status: "running",
      next_sequence: "20",
      terminal_size: {
        columns: 80,
        rows: 24,
        pixel_width: null,
        pixel_height: null,
      },
    },
    input_lease: { held: true, owned_by_client: true },
    layout_lease: { held: true, owned_by_client: true },
    shell_state: null,
    applied_sequence: "12",
    reconnect_sequence: null,
    history_gap: false,
    terminal_size_mismatch: false,
    resize_with_window: true,
    message: null,
    ...overrides,
  };
}

describe("attachment recovery", () => {
  it("uses capped exponential backoff inside the recovery window", () => {
    const backoff = new AttachmentRecoveryBackoff();

    expect(backoff.nextDelay(1_000)).toBe(250);
    expect(backoff.nextDelay(1_250)).toBe(500);
    expect(backoff.nextDelay(1_750)).toBe(1_000);
    expect(backoff.nextDelay(2_750)).toBe(2_000);
    expect(backoff.nextDelay(4_750)).toBe(4_000);
    expect(backoff.nextDelay(8_750)).toBe(5_000);
    expect(backoff.nextDelay(30_900)).toBe(100);
    expect(backoff.nextDelay(1_000 + ATTACHMENT_RECOVERY_WINDOW_MS)).toBeNull();
  });

  it("can be reset after a stable attachment", () => {
    const backoff = new AttachmentRecoveryBackoff();
    expect(backoff.isActive()).toBe(false);
    expect(backoff.nextDelay(1_000)).toBe(250);
    expect(backoff.isActive()).toBe(true);
    expect(backoff.nextDelay(1_250)).toBe(500);

    backoff.reset();

    expect(backoff.isActive()).toBe(false);
    expect(backoff.nextDelay(20_000)).toBe(250);
  });

  it("requires a full recovery window before treating a connection as stable", () => {
    expect(ATTACHMENT_RECOVERY_STABILITY_MS).toBe(
      ATTACHMENT_RECOVERY_WINDOW_MS,
    );
  });

  it("does not retry deterministic terminal failures", () => {
    expect(canAutomaticallyRecoverAttachment("automatic_reconnect_timeout")).toBe(
      false,
    );
    expect(canAutomaticallyRecoverAttachment("explicit_detach_failed")).toBe(
      false,
    );
    expect(canAutomaticallyRecoverAttachment("session_not_found")).toBe(false);
    expect(canAutomaticallyRecoverAttachment("protocol_version_mismatch")).toBe(
      false,
    );
    expect(canAutomaticallyRecoverAttachment("backend_error")).toBe(true);
    expect(canAutomaticallyRecoverAttachment(null)).toBe(true);
  });

  it("falls back to a checkpoint when a resume cursor is ahead", () => {
    expect(reconnectSequenceAfterError("sequence_ahead", "42")).toBeNull();
    expect(reconnectSequenceAfterError("backend_error", "42")).toBe("42");
  });

  it("recovers a lifecycle interruption from a fresh checkpoint", () => {
    const interrupted = interruptedAttachmentState(attachedState());

    expect(interrupted).toMatchObject({
      phase: "disconnected",
      attachment_id: null,
      reconnect_sequence: null,
      resize_with_window: true,
      input_lease: { held: false, owned_by_client: false },
      layout_lease: { held: false, owned_by_client: false },
    });
  });

  it("ignores stale cursors and terminal states after a lifecycle interruption", () => {
    expect(
      interruptedAttachmentState(
        attachedState({ reconnect_sequence: "10", applied_sequence: "12" }),
      )?.reconnect_sequence,
    ).toBeNull();
    expect(interruptedAttachmentState(attachedState({ phase: "ended" }))).toBeNull();
    expect(
      interruptedAttachmentState(
        attachedState({ phase: "idle", session: null }),
      ),
    ).toBeNull();
  });

  it("preserves terminal errors across a lifecycle interruption", () => {
    expect(
      interruptedAttachmentState(
        attachedState({
          phase: "error",
          error_code: "protocol_version_mismatch",
          message: "client and daemon versions differ",
        }),
      ),
    ).toMatchObject({
      phase: "error",
      error_code: "protocol_version_mismatch",
      message: "client and daemon versions differ",
    });
  });
});
