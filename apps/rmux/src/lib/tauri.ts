import { Channel, invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type {
  AttachmentAckRequest,
  AttachmentEvent,
  AttachmentIdRequest,
  AttachmentInputRequest,
  AttachmentLeaseRequest,
  AttachmentResizeRequest,
  CreateSessionRequest,
  ConnectionTarget,
  KillSessionRequest,
  OpenAttachmentRequest,
  OpenAttachmentResponse,
  RestartLocalDaemonResponse,
  SaveSshConfigHostResponse,
  SessionListResponse,
  SessionSummary,
  SshConfigHostCatalog,
  SshHostDefinition,
  SshIdentityFileCatalog,
  SshPrompt,
  WorkspaceDocument,
  WorkspaceSnapshot,
  SessionInspection,
} from "./types";

export async function loadWorkspace(): Promise<WorkspaceSnapshot> {
  return invoke<WorkspaceSnapshot>("load_workspace");
}

export async function updateWorkspace(
  expected_revision: string | null,
  document: WorkspaceDocument,
): Promise<WorkspaceSnapshot> {
  return invoke<WorkspaceSnapshot>("update_workspace", {
    request: { expected_revision, document },
  });
}

export async function probeSshHost(
  target: ConnectionTarget,
  attempt_id: string,
  onPrompt: (prompt: SshPrompt) => void,
): Promise<void> {
  const channel = new Channel<SshPrompt>();
  channel.onmessage = onPrompt;
  await invoke("probe_ssh_host", {
    request: { target, attempt_id },
    on_prompt: channel,
  });
}

export async function respondSshPrompt(
  attempt_id: string,
  prompt_id: string,
  response: string | null,
): Promise<void> {
  await invoke("respond_ssh_prompt", {
    request: { attempt_id, prompt_id, response },
  });
}

export async function cancelSshProbe(attempt_id: string): Promise<void> {
  await invoke("cancel_ssh_probe", { request: { attempt_id } });
}

export async function forgetSshCredentials(
  target: ConnectionTarget,
): Promise<void> {
  await invoke("forget_ssh_credentials", { request: { target } });
}

export interface OpenAttachmentResult {
  attached: OpenAttachmentResponse;
  channel: Channel<AttachmentEvent>;
}

export async function listSessions(
  target: ConnectionTarget,
): Promise<SessionListResponse> {
  const response = await invoke<SessionListResponse>("list_sessions", {
    request: { target },
  });
  return {
    ...response,
    sessions: response.sessions.map((session) => ({ ...session, target })),
  };
}

export async function inspectKnownSessions(
  target: ConnectionTarget,
  session_ids: string[],
): Promise<SessionInspection[]> {
  const results = await invoke<SessionInspection[]>("inspect_known_sessions", {
    request: { target, session_ids },
  });
  return results.map((result) => ({
    ...result,
    session: result.session ? { ...result.session, target } : null,
  }));
}

export async function listSshConfigHosts(): Promise<SshConfigHostCatalog> {
  return invoke<SshConfigHostCatalog>("list_ssh_config_hosts");
}

export async function listSshIdentityFiles(): Promise<SshIdentityFileCatalog> {
  return invoke<SshIdentityFileCatalog>("list_ssh_identity_files");
}

export async function saveSshConfigHost(
  request: SshHostDefinition,
): Promise<SaveSshConfigHostResponse> {
  return invoke<SaveSshConfigHostResponse>("save_ssh_config_host", { request });
}

export async function createSession(
  request: CreateSessionRequest,
): Promise<SessionSummary> {
  const session = await invoke<SessionSummary>("create_session", { request });
  return { ...session, target: request.target };
}

export async function killSession(request: KillSessionRequest): Promise<void> {
  await invoke("kill_session", { request });
}

export async function restartLocalDaemon(): Promise<RestartLocalDaemonResponse> {
  return invoke<RestartLocalDaemonResponse>("restart_local_daemon");
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
  return {
    attached: {
      ...attached,
      session: { ...attached.session, target: request.target },
    },
    channel,
  };
}

export async function sendInput(
  request: AttachmentInputRequest,
): Promise<void> {
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

export async function detachAttachment(
  request: AttachmentIdRequest,
): Promise<void> {
  await invoke("detach_attachment", { request });
}

export async function setNativeWindowTitle(title: string): Promise<void> {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    return;
  }
  await getCurrentWindow().setTitle(title);
}
