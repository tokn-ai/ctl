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
  VpnConnectionInput,
  VpnEnrollmentInput,
  VpnEnrollmentSnapshot,
  TailscaleVpnConnection,
  VpnConnectionsSnapshot,
  VpnStatus,
  VpnSnapshot,
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
import { previewComponentVersions } from "./aboutFixtures";

// This entry is intentionally absent from index.html and the production build.
// The official Tauri mocks intercept every IPC call; nothing reaches a daemon,
// SSH connection, shell process, or the real persisted workspace.
if (!import.meta.env.DEV) throw new Error("The sample workspace is development-only.");

const view_param = new URLSearchParams(location.search).get("view");
const initial_view = view_param === "tasks" || view_param === "ports" || view_param === "vpn" ? view_param : "sessions";
const about_param = new URLSearchParams(location.search).get("about");
let component_versions = previewComponentVersions();
if (about_param === "partial") {
  const daemon = component_versions.components.find((row) => row.component === "rmuxd")!;
  daemon.running = null;
  daemon.status = "unavailable";
  daemon.error = "The running rmuxd did not respond before the check timed out.";
}
if (about_param === "legacy") {
  const daemon = component_versions.components.find((row) => row.component === "taskd")!;
  daemon.running = { version: null, source_revision: null, source_fingerprint: null, dirty: null, protocols: [{ name: "task", version: 3 }] };
  daemon.status = "unknown";
  daemon.detail = "The running daemon reports its protocol but no build identity.";
  const agent = component_versions.components.find((row) => row.component === "ctl_agent")!;
  agent.running = { ...agent.running!, source_fingerprint: null };
  agent.status = "unknown";
  agent.detail = "The observed revision does not provide enough evidence to verify this build.";
}
let workspace: WorkspaceSnapshot = { revision: "preview-1", document: previewWorkspace(initial_view) };
const connectedHosts = new Set(["dev-server"]);
const pausedHosts = new Set<string>();
const connectionKey = (target: ConnectionTarget) => target.kind === "ssh" ? target.destination : "local";
let hosts: HostCatalogSnapshot = { revision: "preview-hosts-1", document: previewHostCatalog() };
const ssh_config_hosts = [{ destination: "dev-server" }, { destination: "staging" }, { destination: "research" }];
let revision = 1;
let sessions = structuredClone(previewSessions);
let tasks = structuredClone(previewTasks);
let definitions = structuredClone(previewDefinitions);
let keybindings: KeybindingsDocument = { schema_version: 1, overrides: [] };
const attachments = new Map<string, { channel: Channel<AttachmentEvent>; sequence: number; session: SessionSummary }>();
const forwards = new Map<string, PortForwardStatus>();
let vpn_connections: VpnConnectionsSnapshot = {
  revision: "preview-vpn-1",
  connections: [{
    connection_id: "sample-vpn", name: "Office", url: "https://vpn.example.com",
    username: "sample", has_password: true, auth_method: null, target_ip: null,
  }, {
    connection_id: "research-vpn", name: "Research", url: "https://research.example.com",
    username: "researcher", has_password: true, auth_method: null, target_ip: null,
  }, {
    provider: "tailscale", connection_id: "tailnet-vpn", name: "Tailnet", hostname: "rmux-preview", accept_routes: false,
  }],
};
const stopped_vpn: VpnStatus = {
  state: "stopped", running: false, connection_id: null, endpoint: null, container_name: null,
  vpn_url: null, username: null,
};
const vpn_param = new URLSearchParams(location.search).get("vpn");
let vpn_snapshot: VpnSnapshot = { supports_multiple: vpn_param !== "legacy", supports_tailscale_enrollment: vpn_param !== "legacy", supported_providers: vpn_param === "legacy" ? ["openconnect"] : ["openconnect", "tailscale"], connections: [] };
const signed_in_tailnets = new Set<string>();
const vpn_enrollments = new Map<string, { connection: TailscaleVpnConnection; created_at: number; adopted: boolean }>();
const enrollment_param = new URLSearchParams(location.search).get("enrollment");
let next_vpn_port = 49160;
if (["connected", "external", "legacy", "multiple"].includes(vpn_param ?? "")) {
  const external = vpn_param === "external" || vpn_param === "legacy";
  vpn_snapshot.connections.push({
    vpn_id: external ? "preview-external" : "sample-vpn", state: "connected", running: true,
    connection_id: external ? null : "sample-vpn", vpn_url: "https://vpn.example.com", username: "sample",
    endpoint: "socks5h://127.0.0.1:49152", container_name: "preview-vpn",
  });
}
if (vpn_param === "multiple") {
  vpn_snapshot.connections.push({
    vpn_id: "research-external", state: "connected", running: true, connection_id: null,
    vpn_url: "https://research.example.com", username: "researcher",
    endpoint: "socks5h://127.0.0.1:49153", container_name: "preview-research-vpn",
  });
}

if (vpn_param === "tailscale-sign-in" || vpn_param === "tailscale-connected") {
  const connected = vpn_param === "tailscale-connected";
  if (connected) signed_in_tailnets.add("tailnet-vpn");
  vpn_snapshot.connections.push({
    provider: "tailscale", vpn_id: "tailnet-vpn", connection_id: "tailnet-vpn",
    state: connected ? "connected" : "starting", running: connected, hostname: "rmux-preview",
    tailnet: connected ? "example.test" : null, username: connected ? "sample@example.test" : null,
    auth_url: connected ? null : "https://login.tailscale.com/a/examplePreview",
    message: connected ? null : "Sign in to finish connecting.",
    endpoint: connected ? "socks5h://127.0.0.1:49154" : null, container_name: "preview-tailscale",
  });
}

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
    case "get_component_versions":
      return structuredClone(component_versions);
    case "preflight_component_action": {
      const { component_id } = request<{ component_id: string }>(payload);
      const row = component_versions.components.find((item) => item.component_id === component_id);
      if (!row?.action) throw new Error("This component does not support a managed action.");
      if (row.component === "taskd" && tasks.some((task) => task.active_run)) throw new Error("Stop active tasks before restarting taskd.");
      const terminal_sessions = row.component === "rmuxd" ? sessions.filter((session) => (session.target.kind === "local") === (row.location === "local")).length : null;
      const description = row.component === "ctld" ? `Stops ${vpn_snapshot.connections.length} managed VPN connections and may interrupt SSH connections and port forwards.`
        : row.component === "ctl_agent" ? "Reconnects this app’s terminal connections to this remote environment. Remote sessions keep running."
        : row.component === "taskd" ? "Restarts taskd. Saved task definitions and drafts are retained."
        : row.location === "local" ? "Terminates every local rmux session, including sessions opened by other apps. This cannot be undone."
        : "Terminates every rmux session for this remote environment, including sessions opened by other clients. This cannot be undone.";
      return { action_token: `preview-action-${component_id}`, component_id, component: row.component, location: row.location, host_id: row.host_id, label: row.label, action: row.action, running: row.running, available: row.available,
        impact: { ssh_connections: null, port_forwards: null, vpn_connections: row.component === "ctld" ? vpn_snapshot.connections.length : null, terminal_sessions, description } };
    }
    case "execute_component_action": {
      const { action_token } = request<{ action_token: string }>(payload);
      const row = component_versions.components.find((item) => `preview-action-${item.component_id}` === action_token);
      if (!row?.action) throw new Error("Check the component again before continuing.");
      return new Promise((resolve, reject) => setTimeout(() => {
        if (about_param === "restart-error") { reject(new Error("The replacement did not become ready. Refresh versions to check its state.")); return; }
        component_versions = { components: component_versions.components.map((item) => item.component_id === row.component_id ? { ...item, running: item.available, status: "current" } : item) };
        if (row.component === "ctld") vpn_snapshot.connections = [];
        resolve({ component_id: row.component_id, component: row.component, location: row.location, host_id: row.host_id, action: row.action, running: row.available, detail: null });
      }, 1200));
    }
    case "ack_component_reconnect":
      return;
    case "load_workspace":
      return structuredClone(workspace);
    case "update_workspace":
      workspace = { revision: `preview-${++revision}`, document: request<{ document: WorkspaceDocument }>(payload).document };
      return structuredClone(workspace);
    case "load_hosts":
      return structuredClone(hosts);
    case "load_vpn_connections":
      return structuredClone(vpn_connections);
    case "begin_vpn_enrollment": {
      const input = request<VpnEnrollmentInput>(payload);
      const enrollment_id = crypto.randomUUID();
      const connection_id = crypto.randomUUID();
      const connection: TailscaleVpnConnection = { ...input, provider: "tailscale", connection_id };
      vpn_enrollments.set(enrollment_id, { connection, created_at: Date.now(), adopted: false });
      const status: VpnStatus = { ...stopped_vpn, provider: "tailscale", vpn_id: connection_id, connection_id,
        state: "starting", hostname: input.hostname ?? "rmux-preview", message: "Starting Tailscale…" };
      vpn_snapshot.connections.push(status);
      return { enrollment_id, connection_id, status, error: null } satisfies VpnEnrollmentSnapshot;
    }
    case "vpn_enrollment_status": {
      const { enrollment_id } = request<{ enrollment_id: string }>(payload);
      const draft = vpn_enrollments.get(enrollment_id);
      if (!draft) throw new Error("This sign-in was canceled.");
      const status = vpn_snapshot.connections.find((item) => item.connection_id === draft.connection.connection_id)!;
      if (enrollment_param === "failure") return { enrollment_id, connection_id: draft.connection.connection_id, status,
        error: { code: "vpn_failed", message: "Could not start Tailscale. Check that your container engine is running." } };
      if (Date.now() - draft.created_at > 1500 && status.state !== "connected" && enrollment_param !== "starting") {
        status.auth_url = "https://login.tailscale.com/a/examplePreview";
        status.message = "Sign in to Tailscale in your browser";
      }
      return structuredClone({ enrollment_id, connection_id: draft.connection.connection_id, status, error: null } satisfies VpnEnrollmentSnapshot);
    }
    case "save_vpn_enrollment": {
      const { enrollment_id } = request<{ enrollment_id: string }>(payload);
      const draft = vpn_enrollments.get(enrollment_id);
      if (!draft) throw new Error("This sign-in was canceled.");
      const status = vpn_snapshot.connections.find((item) => item.connection_id === draft.connection.connection_id);
      if (status?.state !== "connected") throw new Error("Finish signing in before saving this connection.");
      if (!draft.adopted) {
        vpn_connections = { revision: `preview-vpn-${++revision}`, connections: [...vpn_connections.connections, draft.connection] };
        draft.adopted = true;
      }
      return structuredClone(vpn_connections);
    }
    case "cancel_vpn_enrollment": {
      const { enrollment_id } = request<{ enrollment_id: string }>(payload);
      const draft = vpn_enrollments.get(enrollment_id);
      if (draft && !draft.adopted) {
        vpn_snapshot.connections = vpn_snapshot.connections.filter((item) => item.connection_id !== draft.connection.connection_id);
        vpn_enrollments.delete(enrollment_id);
      }
      return;
    }
    case "save_vpn_connection": {
      const { connection } = request<{ connection: VpnConnectionInput }>(payload);
      const prior = vpn_connections.connections.find((item) => item.connection_id === connection.connection_id);
      const summary = connection.provider === "tailscale" ? connection : (() => {
        const { password, ...settings } = connection;
        return { ...settings, has_password: password !== null || (prior?.provider !== "tailscale" && prior?.has_password === true) };
      })();
      vpn_connections = {
        revision: `preview-vpn-${++revision}`,
        connections: [
          ...vpn_connections.connections.filter((item) => item.connection_id !== connection.connection_id),
          summary,
        ],
      };
      return structuredClone(vpn_connections);
    }
    case "delete_vpn_connection": {
      const { connection_id } = request<{ connection_id: string }>(payload);
      vpn_connections = {
        revision: `preview-vpn-${++revision}`,
        connections: vpn_connections.connections.filter((item) => item.connection_id !== connection_id),
      };
      return structuredClone(vpn_connections);
    }
    case "connect_vpn": {
      const { connection_id } = request<{ connection_id: string }>(payload);
      const connection = vpn_connections.connections.find((item) => item.connection_id === connection_id);
      if (!connection) throw new Error("This sample VPN connection no longer exists.");
      if (!vpn_snapshot.supports_multiple && vpn_snapshot.connections.length > 0) throw new Error("Update ctld to connect multiple VPNs.");
      const tailscale = connection.provider === "tailscale";
      const connected = !tailscale || signed_in_tailnets.has(connection_id);
      const status: VpnStatus = {
        provider: connection.provider ?? "openconnect",
        vpn_id: connection_id, state: connected ? "connected" : "starting", running: connected, connection_id,
        ...(tailscale ? { hostname: connection.hostname, auth_url: connected ? null : "https://login.tailscale.com/a/examplePreview" }
          : { vpn_url: connection.url, username: connection.username }),
        endpoint: connected ? `socks5h://127.0.0.1:${next_vpn_port++}` : null, container_name: `preview-${connection_id}`,
      };
      vpn_snapshot.connections = [...vpn_snapshot.connections.filter((item) => item.vpn_id !== connection_id), status];
      return structuredClone(status);
    }
    case "open_vpn_sign_in": {
      const { vpn_id } = request<{ vpn_id: string }>(payload);
      if (enrollment_param === "browser-error") throw new Error("Could not open the browser for VPN sign-in.");
      if (enrollment_param === "waiting") return;
      signed_in_tailnets.add(vpn_id);
      vpn_snapshot.connections = vpn_snapshot.connections.map((status) => status.vpn_id === vpn_id ? {
        ...status, state: "connected", running: true, auth_url: null, message: null,
        tailnet: "example.test", username: "sample@example.test", endpoint: `socks5h://127.0.0.1:${next_vpn_port++}`,
      } : status);
      return;
    }
    case "vpn_status":
      return structuredClone(vpn_snapshot);
    case "stop_vpn": {
      const { vpn_id } = request<{ vpn_id: string }>(payload);
      vpn_snapshot.connections = vpn_snapshot.connections.filter((item) => item.vpn_id !== vpn_id);
      return { ...stopped_vpn, vpn_id };
    }
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
      document.title = `${(payload as { value: string }).value} · Sample workspace preview`;
      return;
    case "session_cache":
      return request<{ action: { kind: string } }>(payload).action.kind === "load"
        ? { kind: "loaded", cache: null }
        : { kind: "archived" };
    case "session_view":
      return null;
    case "list_ssh_config_hosts":
      return { hosts: structuredClone(ssh_config_hosts), warnings: [] };
    case "list_ssh_identity_files":
      return { identity_files: [{ path: "/sample/.ssh/id_ed25519", display_path: "~/.ssh/id_ed25519" }], warnings: [] };
    case "probe_ssh_host": {
      const { target } = request<{ target: ConnectionTarget }>(payload);
      connectedHosts.add(connectionKey(target));
      pausedHosts.delete(connectionKey(target));
      const configured = previewTargets.find((known) => sameTarget(known, target));
      return configured?.kind === "ssh" ? configured.remote_info : {
        remote_id: `sample-${target.kind === "ssh" ? target.destination : "local"}`, agent_version: "0.1.0",
      };
    }
    case "ssh_connection_status": {
      const { target } = request<{ target: ConnectionTarget }>(payload);
      return { connected: connectedHosts.has(connectionKey(target)), manually_disconnected: pausedHosts.has(connectionKey(target)) };
    }
    case "disconnect_ssh_host": {
      const { targets } = request<{ targets: ConnectionTarget[] }>(payload);
      for (const target of targets) {
        connectedHosts.delete(connectionKey(target));
        pausedHosts.add(connectionKey(target));
      }
      return;
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
      const { target, forward, enabled } = request<{ target: ConnectionTarget; forward: LocalPortForward; enabled: boolean }>(payload);
      const connected = connectedHosts.has(connectionKey(target));
      const status: PortForwardStatus = { forward, state: connected ? "active" : "waiting_for_authentication", message: connected ? null : "Connect this host to activate the forward." };
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
