import { createRoot } from "react-dom/client";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Channel, InvokeArgs } from "@tauri-apps/api/core";
import type {
  AttachmentEvent,
  AttachmentIdRequest,
  AttachmentInputRequest,
  AttachmentLeaseRequest,
  AttachmentResizeRequest,
  ConnectionTarget,
  CreateSessionRequest,
  HostCatalogDocument,
  HostCatalogSnapshot,
  KeybindingsDocument,
  LocalPortForward,
  OpenAttachmentRequest,
  PortForwardStatus,
  SavedTaskDefinition,
  SessionSummary,
  TaskDefinition,
  TaskDefinitionScope,
  TaskLogEvent,
  TaskRequest,
  WorkspaceDocument,
  WorkspaceSnapshot,
} from "../src/lib/types";
import {
  previewDefinitions,
  previewHostCatalog,
  previewOutput,
  previewSessions,
  previewShell,
  previewTasks,
  previewTargets,
  previewWorkspace,
} from "./fixtures";

// This entry is intentionally absent from index.html and the production build.
// The official Tauri mocks intercept every IPC call; nothing reaches a daemon,
// SSH connection, shell process, or the real persisted workspace.
if (!import.meta.env.DEV) throw new Error("The sample workspace is development-only.");

const view_param = new URLSearchParams(location.search).get("view");
const initial_view = view_param === "tasks" || view_param === "ports" ? view_param : "sessions";
let workspace: WorkspaceSnapshot = { revision: "preview-1", document: previewWorkspace(initial_view) };
let hosts: HostCatalogSnapshot = { revision: "preview-hosts-1", document: previewHostCatalog() };
const ssh_config_hosts = [{ destination: "dev-server" }, { destination: "staging" }, { destination: "research" }];
let revision = 1;
let sessions = structuredClone(previewSessions);
let tasks = structuredClone(previewTasks);
let definitions = structuredClone(previewDefinitions);
let keybindings: KeybindingsDocument = { schema_version: 1, overrides: [] };
const attachments = new Map<string, { channel: Channel<AttachmentEvent>; sequence: number; session: SessionSummary }>();
const forwards = new Map<string, PortForwardStatus>();

function request<T>(payload: InvokeArgs | undefined): T {
  return (payload as { request: T }).request;
}

function channel<T>(payload: InvokeArgs | undefined, key: string): Channel<T> {
  return (payload as Record<string, unknown>)[key] as Channel<T>;
}

function sameTarget(left: ConnectionTarget, right: ConnectionTarget): boolean {
  return left.kind === "local"
    ? right.kind === "local"
    : right.kind === "ssh" && left.destination === right.destination;
}

function emitOutput(attachment_id: string, text: string): void {
  const attachment = attachments.get(attachment_id);
  if (!attachment) return;
  const bytes = new TextEncoder().encode(text);
  const start = attachment.sequence;
  attachment.sequence += bytes.length;
  attachment.channel.onmessage({
    event_type: "output",
    attachment_id,
    event_id: `${attachment_id}:${attachment.sequence}`,
    sequence_start: String(start),
    sequence_end: String(attachment.sequence),
    data_base64: btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join("")),
  });
}

function requireSession(session_id: string): SessionSummary {
  const session = sessions.find((item) => item.session_id === session_id);
  if (!session) throw new Error("This sample session no longer exists. Reload to reset the preview.");
  return session;
}

mockWindows("main");
mockIPC((command, payload) => {
  switch (command) {
    case "load_workspace":
      return structuredClone(workspace);
    case "update_workspace":
      workspace = { revision: `preview-${++revision}`, document: request<{ document: WorkspaceDocument }>(payload).document };
      return structuredClone(workspace);
    case "load_hosts":
      return structuredClone(hosts);
    case "update_hosts":
      hosts = { revision: `preview-hosts-${++revision}`, document: request<{ document: HostCatalogDocument }>(payload).document };
      return structuredClone(hosts);
    case "load_keybindings":
      return { path: "Sample workspace", revision: "1", document: keybindings };
    case "save_keybindings":
      keybindings = request<{ document: KeybindingsDocument }>(payload).document;
      return { path: "Sample workspace", revision: String(++revision), document: keybindings };
    case "sync_command_menu":
    case "acknowledge_attachment_event":
    case "cancel_attachment_open":
    case "acknowledge_task_log":
    case "cancel_task_logs":
    case "forget_ssh_credentials":
    case "cancel_ssh_probe":
    case "respond_ssh_prompt":
      return;
    case "plugin:window|set_title":
      document.title = `${(payload as { title: string }).title} · Sample workspace preview`;
      return;
    case "list_ssh_config_hosts":
      return { hosts: structuredClone(ssh_config_hosts), warnings: [] };
    case "list_ssh_identity_files":
      return { identity_files: [{ path: "/sample/.ssh/id_ed25519", display_path: "~/.ssh/id_ed25519" }], warnings: [] };
    case "probe_ssh_host": {
      const { target } = request<{ target: ConnectionTarget }>(payload);
      const configured = previewTargets.find((known) => sameTarget(known, target));
      return configured?.kind === "ssh" ? configured.remote_info : {
        remote_id: `sample-${target.kind === "ssh" ? target.destination : "local"}`, agent_version: "0.1.0",
      };
    }
    case "list_sessions": {
      const { target } = request<{ target: ConnectionTarget }>(payload);
      const matching = sessions.filter((session) => sameTarget(session.target, target));
      return { sessions: matching, shell_states: Object.fromEntries(matching.map((session) => [session.session_id, previewShell(session)])) };
    }
    case "inspect_known_sessions": {
      const { session_ids } = request<{ session_ids: string[] }>(payload);
      return session_ids.map((session_id) => {
        const session = sessions.find((item) => item.session_id === session_id) ?? null;
        return { session_id, session, shell_state: session ? previewShell(session) : null, error: null };
      });
    }
    case "create_session": {
      const { target, terminal_size } = request<CreateSessionRequest>(payload);
      const session: SessionSummary = { target, terminal_size, session_id: `sample-shell-${++revision}`, name: "zsh", status: "running", next_sequence: "0" };
      sessions.push(session);
      return session;
    }
    case "kill_session": {
      const { session_id } = request<{ session_id: string }>(payload);
      sessions = sessions.filter((session) => session.session_id !== session_id);
      return;
    }
    case "open_attachment": {
      const data = request<OpenAttachmentRequest>(payload);
      const session = requireSession(data.session);
      const attachment_id = `sample-attachment-${++revision}`;
      const on_event = channel<AttachmentEvent>(payload, "on_event");
      channel<string>(payload, "on_opening").onmessage(attachment_id);
      attachments.set(attachment_id, { channel: on_event, sequence: Number(data.resume_from ?? 0), session });
      if (data.resume_from === null) emitOutput(attachment_id, previewOutput(session));
      return {
        attachment_id,
        session: { ...session, terminal_size: data.terminal_size },
        replay_from: data.resume_from ?? "0",
        history_gap: false,
        terminal_size_mismatch: false,
        input_lease: { held: true, owned_by_client: true },
        layout_lease: { held: data.request_layout_lease, owned_by_client: data.request_layout_lease },
        shell_state: previewShell(session),
      };
    }
    case "detach_attachment":
      attachments.delete(request<AttachmentIdRequest>(payload).attachment_id);
      return;
    case "send_input": {
      const data = request<AttachmentInputRequest>(payload);
      const input = atob(data.data_base64);
      // Deliberately a terminal echo, never command execution.
      emitOutput(data.attachment_id, input.replace(/\r/g, "\r\n\x1b[90mPreview input only · no command executed\x1b[0m\r\n\x1b[32m❯\x1b[0m "));
      return;
    }
    case "resize_attachment": {
      const data = request<AttachmentResizeRequest>(payload);
      const attachment = attachments.get(data.attachment_id);
      attachment?.channel.onmessage({ event_type: "pty_geometry_changed", attachment_id: data.attachment_id, event_id: `geometry-${++revision}`, terminal_size: data.terminal_size, observed_sequence: String(attachment.sequence) });
      return;
    }
    case "acquire_attachment_lease":
    case "release_attachment_lease": {
      const data = request<AttachmentLeaseRequest>(payload);
      const owned = command === "acquire_attachment_lease";
      attachments.get(data.attachment_id)?.channel.onmessage({ event_type: "lease_status", attachment_id: data.attachment_id, lease: data.lease, status: { held: owned, owned_by_client: owned } });
      return;
    }
    case "load_task_definitions":
      return { scope: request<{ scope: TaskDefinitionScope }>(payload).scope, path: "Sample definitions", definitions: structuredClone(definitions) };
    case "save_task_definition": {
      const data = request<{ definition_id: string; definition: TaskDefinition }>(payload);
      const saved: SavedTaskDefinition = { ...data, revision: String(++revision) };
      definitions = [...definitions.filter((item) => item.definition_id !== saved.definition_id), saved];
      return saved;
    }
    case "remove_task_definition":
      definitions = definitions.filter((item) => item.definition_id !== request<{ definition_id: string }>(payload).definition_id);
      return;
    case "task_request": {
      const data = request<TaskRequest>(payload);
      if (data.type === "list_tasks") return { type: "task_list", tasks: structuredClone(tasks) };
      if (data.type === "register_task") {
        const task = { task_id: data.task_id, definition: data.definition, desired_state: "stopped" as const, active_run: null, last_run: null };
        tasks.push(task);
        return { type: "task_created", task };
      }
      const task = tasks.find((item) => item.task_id === data.task);
      if (!task) throw new Error("Sample task not found. Reload to reset the preview.");
      if (data.type === "remove_task") {
        tasks = tasks.filter((item) => item.task_id !== data.task);
        return { type: "task_removed", task_id: data.task };
      }
      if (data.type === "update_task") task.definition = data.definition;
      if (data.type === "stop_task") {
        task.desired_state = "stopped";
        task.last_run = task.active_run ? { ...task.active_run, state: "stopped", ended_at_ms: 1790000009000, exit_code: 0 } : task.last_run;
        task.active_run = null;
      }
      if (data.type === "start_task" || data.type === "restart_task") {
        task.desired_state = "running";
        task.active_run = { run_id: `sample-run-${++revision}`, state: "running", started_at_ms: 1790000009000, ended_at_ms: null, exit_code: null, definition: task.definition };
      }
      return { type: "task_status", task: structuredClone(task) };
    }
    case "watch_task_logs": {
      const task = tasks.find((item) => item.task_id === request<{ task_id: string }>(payload).task_id);
      const subscription_id = `sample-logs-${++revision}`;
      const run = task?.active_run ?? task?.last_run;
      if (run) {
        const output = task?.task_id === "preview-task-0"
          ? "Sample workspace · browser preview\n\n> rmux-app@0.1.0 dev\n> vite\n\n  VITE v7.0.4  ready in 184 ms\n\n  ➜  Local:   http://localhost:1430/\n  ➜  Network: use --host to expose\n\n  10:42:18 [vite] hmr update /src/App.css\n  10:42:20 [vite] hmr update /src/components/sessions/SessionSidebar.tsx\n"
          : "Sample workspace · browser preview\n\nAll checks passed.\nProcess exited with code 0.\n";
        channel<TaskLogEvent>(payload, "onEvent").onmessage({ event_type: "log", subscription_id, run_id: run.run_id, sequence: "1", stream: "stdout", data: Array.from(new TextEncoder().encode(output)) });
      }
      return subscription_id;
    }
    case "list_port_forwards": {
      const { target } = request<{ target: ConnectionTarget }>(payload);
      const host_id = target.kind === "ssh" ? target.host_id : "local";
      const ids = new Set(workspace.document.port_forwards?.filter((forward) => forward.host_id === host_id).map((forward) => forward.forward_id));
      return [...forwards.values()].filter((status) => ids.has(status.forward.forward_id));
    }
    case "configure_port_forward": {
      const { forward, enabled } = request<{ forward: LocalPortForward; enabled: boolean }>(payload);
      const status: PortForwardStatus = { forward, state: "active", message: null };
      if (enabled) forwards.set(forward.forward_id, status);
      else forwards.delete(forward.forward_id);
      return status;
    }
    case "list_remote_listeners":
      return { listeners: [5432, 8080, 9090].map((port) => ({ bind_address: "127.0.0.1", port })), warnings: [] };
    case "check_local_port":
      return { port: request<{ port: number }>(payload).port, available: true, message: null };
    case "save_ssh_config_host": {
      const { alias } = request<{ alias: string }>(payload);
      if (!ssh_config_hosts.some((host) => host.destination === alias)) ssh_config_hosts.push({ destination: alias });
      return { destination: alias };
    }
    case "restart_local_daemon":
      return { terminated_sessions: sessions.filter((session) => session.target.kind === "local").length };
    case "restart_task_daemon":
      return;
    default:
      throw new Error(`The browser preview does not simulate ${command}.`);
  }
}, { shouldMockEvents: true });

// Dynamic import ensures the native window API sees mockWindows first.
const { default: App } = await import("../src/App");
const { AppErrorBoundary } = await import("../src/components/errors/AppErrorBoundary");
createRoot(document.getElementById("root")!).render(<AppErrorBoundary><App /></AppErrorBoundary>);
