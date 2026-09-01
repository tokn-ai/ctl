import type { AttachmentViewState } from "../../lib/types";

export const INITIAL_ATTACHMENT_RECOVERY_DELAY_MS = 250;
export const MAX_ATTACHMENT_RECOVERY_DELAY_MS = 5_000;
export const ATTACHMENT_RECOVERY_WINDOW_MS = 30_000;

const EMPTY_LEASE = { held: false, owned_by_client: false };

const NON_RECOVERABLE_ERROR_CODES = new Set([
  "automatic_reconnect_timeout",
  "explicit_detach_failed",
  "invalid_request",
  "invalid_session_name",
  "invalid_terminal_size",
  "protocol_version_mismatch",
  "session_not_found",
  "unsupported_platform",
]);

export function canAutomaticallyRecoverAttachment(
  errorCode: string | null,
): boolean {
  return errorCode === null || !NON_RECOVERABLE_ERROR_CODES.has(errorCode);
}

export function reconnectSequenceAfterError(
  errorCode: string | null,
  reconnectSequence: string | null,
): string | null {
  return errorCode === "sequence_ahead" ? null : reconnectSequence;
}

/**
 * Converts a mounted attachment into a resumable disconnected snapshot.
 *
 * A lifecycle interruption can occur while the renderer is applying an event,
 * before it advances the acknowledged cursor. Replaying from the previous
 * cursor could therefore duplicate partially applied output, so this path
 * deliberately requests a fresh checkpoint. Normal connection exits use the
 * daemon-provided safe cursor instead.
 */
export function interruptedAttachmentState(
  state: AttachmentViewState,
): AttachmentViewState | null {
  if (!state.session || state.phase === "idle" || state.phase === "ended") {
    return null;
  }

  const recoverable = canAutomaticallyRecoverAttachment(state.error_code);

  return {
    ...state,
    phase: recoverable ? "disconnected" : state.phase,
    attachment_id: null,
    input_lease: EMPTY_LEASE,
    layout_lease: EMPTY_LEASE,
    reconnect_sequence: recoverable ? null : state.reconnect_sequence,
    message: recoverable
      ? "Attachment interrupted. Reconnecting automatically."
      : state.message,
  };
}

export class AttachmentRecoveryBackoff {
  private startedAt: number | null = null;
  private attempt = 0;

  nextDelay(now: number): number | null {
    this.startedAt ??= now;
    const elapsed = now - this.startedAt;
    const remaining = ATTACHMENT_RECOVERY_WINDOW_MS - elapsed;
    if (remaining <= 0) {
      return null;
    }

    const delay = Math.min(
      INITIAL_ATTACHMENT_RECOVERY_DELAY_MS * 2 ** this.attempt,
      MAX_ATTACHMENT_RECOVERY_DELAY_MS,
      remaining,
    );
    this.attempt += 1;
    return delay;
  }

  reset(): void {
    this.startedAt = null;
    this.attempt = 0;
  }
}
