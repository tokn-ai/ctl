import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AttachmentAckRequest,
  AttachmentEvent,
  AttachmentIdRequest,
  AttachmentInputRequest,
  AttachmentLeaseRequest,
  AttachmentResizeRequest,
  CreateSessionRequest,
  OpenAttachmentRequest,
  OpenAttachmentResponse,
  SessionSummary,
} from "./types";

export interface OpenAttachmentResult {
  attached: OpenAttachmentResponse;
  channel: Channel<AttachmentEvent>;
}

export async function listSessions(): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>("list_sessions");
}

export async function createSession(
  request: CreateSessionRequest,
): Promise<SessionSummary> {
  return invoke<SessionSummary>("create_session", { request });
}

export async function openAttachment(
  request: OpenAttachmentRequest,
  onEvent: (event: AttachmentEvent) => void,
): Promise<OpenAttachmentResult> {
  const channel = new Channel<AttachmentEvent>();
  channel.onmessage = onEvent;
  const attached = await invoke<OpenAttachmentResponse>("open_attachment", {
    request,
    onEvent: channel,
  });
  return { attached, channel };
}

export async function sendInput(request: AttachmentInputRequest): Promise<void> {
  await invoke("send_input", { request });
}

export async function resizeAttachment(
  request: AttachmentResizeRequest,
): Promise<void> {
  await invoke("resize_attachment", { request });
}

export async function acquireAttachmentLease(
  request: AttachmentLeaseRequest,
): Promise<void> {
  await invoke("acquire_attachment_lease", { request });
}

export async function releaseAttachmentLease(
  request: AttachmentLeaseRequest,
): Promise<void> {
  await invoke("release_attachment_lease", { request });
}

export async function acknowledgeAttachmentEvent(
  request: AttachmentAckRequest,
): Promise<void> {
  await invoke("acknowledge_attachment_event", { request });
}

export async function detachAttachment(request: AttachmentIdRequest): Promise<void> {
  await invoke("detach_attachment", { request });
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return error.message;
  }
  return "An unexpected rmux error occurred.";
}
