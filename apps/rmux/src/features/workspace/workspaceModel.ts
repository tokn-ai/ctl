import type {
  WorkspaceTab,
  TaskTab,
  SavedTaskDefinition,
  TaskDefinitionDraft,
  TaskDefinitionScope,
  TaskReference,
  ConnectionTarget,
  SessionReference,
  SessionSummary,
  ShellStateSummary,
  WorkspaceDocument,
  WorkspacePortForward,
  WorkspaceSidebarView,
  WorkspaceSshGateway,
  SshConnectionTarget,
  WorkspaceHost,
  LegacyWorkspaceHost,
} from "../../lib/types";
import { LOCAL_TARGET, sessionKey, targetKey } from "../targets/targets";

export interface WorkspaceView {
  sidebar_view: WorkspaceSidebarView;
  task_drafts: TaskDefinitionDraft[];
  task_definitions: SavedTaskDefinition[];
  task_definition_scope: TaskDefinitionScope;
  task_references: TaskReference[];
  task_tabs: TaskTab[];
  tab_order: string[];
  targets: ConnectionTarget[];
  hosts: WorkspaceHost[];
  sessions: SessionSummary[];
  tabs: SessionSummary[];
  active_tab_key: string | null;
  shell_states: ReadonlyMap<string, ShellStateSummary>;
  port_forwards: WorkspacePortForward[];
  ssh_gateways: WorkspaceSshGateway[];
}

export function emptyWorkspaceView(): WorkspaceView {
  return {
    sidebar_view: "sessions",
    task_drafts: [],
    task_definitions: [],
    task_definition_scope: { kind: "global" },
    task_references: [],
    task_tabs: [],
    tab_order: [],
    targets: [LOCAL_TARGET],
    hosts: [hostFromTarget(LOCAL_TARGET)],
    sessions: [],
    tabs: [],
    active_tab_key: null,
    shell_states: new Map(),
    port_forwards: [],
    ssh_gateways: [],
  };
}

export function withHostId(target: ConnectionTarget): ConnectionTarget {
  if (target.kind === "local") return LOCAL_TARGET;
  return { ...target, host_id: target.host_id ?? crypto.randomUUID() };
}

/** Keep transport details out of the host's stable identity. */
export function connectionSettings(target: SshConnectionTarget): SshConnectionTarget {
  return {
    kind: "ssh",
    destination: target.destination,
    ...(target.hostname ? { hostname: target.hostname } : {}),
    ...(target.user ? { user: target.user } : {}),
    ...(target.port ? { port: target.port } : {}),
    ...(target.identity_file ? { identity_file: target.identity_file } : {}),
    ...(target.gateway_route?.length ? { gateway_route: target.gateway_route } : {}),
  };
}

export function hostFromTarget(target: ConnectionTarget, name?: string): WorkspaceHost {
  if (target.kind === "local") return {
    host_id: "local", name: "Local", connection_methods: [], preferred_method_id: null,
  };
  return {
    host_id: target.host_id ?? crypto.randomUUID(),
    name: name ?? target.host_name ?? target.destination,
    remote_info: target.remote_info,
    connection_methods: [{ method_id: "default", name: "SSH", target: connectionSettings(target) }],
    preferred_method_id: "default",
  };
}

export function normalizeWorkspaceHost(host: WorkspaceHost | LegacyWorkspaceHost): WorkspaceHost {
  return "target" in host
    ? hostFromTarget(host.target.kind === "local" ? host.target : { ...host.target, host_id: host.host_id })
    : host;
}

export function hostTarget(
  host: WorkspaceHost,
  gateways: readonly WorkspaceSshGateway[],
  method_id = host.preferred_method_id,
): ConnectionTarget {
  if (host.host_id === "local") return LOCAL_TARGET;
  const method = host.connection_methods.find((item) => item.method_id === method_id);
  if (!method) throw new Error(`Choose a connection method for ${host.name}.`);
  return resolveSshGateways({
    ...connectionSettings(method.target),
    host_id: host.host_id,
    host_name: host.name,
    method_id: method.method_id,
    ...(host.remote_info ? { remote_info: host.remote_info } : {}),
  }, gateways);
}

/** Editing metadata never changes the transport used by existing sessions. */
export function updateHostSettings(view: WorkspaceView, host: WorkspaceHost): WorkspaceView {
  if (!view.hosts.some((known) => known.host_id === host.host_id))
    throw new Error("This host is no longer in the workspace.");
  const rename = (target: ConnectionTarget): ConnectionTarget =>
    target.kind === "ssh" && target.host_id === host.host_id
      ? { ...target, host_name: host.name }
      : target;
  return {
    ...view,
    hosts: view.hosts.map((known) => known.host_id === host.host_id ? host : known),
    targets: view.targets.map(rename),
    sessions: view.sessions.map((session) => ({ ...session, target: rename(session.target) })),
    tabs: view.tabs.map((session) => ({ ...session, target: rename(session.target) })),
  };
}

export function resolveSshGateways(
  target: SshConnectionTarget,
  gateways: readonly WorkspaceSshGateway[],
): SshConnectionTarget {
  const byId = new Map(
    gateways.map((gateway) => [gateway.gateway_id, gateway]),
  );
  const { gateways: _current, ...persistedTarget } = target;
  const resolved = (target.gateway_route ?? []).map((step) => {
    const gateway = byId.get(step.gateway_id);
    if (!gateway) throw new Error("A gateway for this connection method is missing. Edit the method before connecting.");
    return { ...gateway, mode: step.mode };
  });
  return resolved.length > 0
    ? { ...persistedTarget, gateways: resolved }
    : persistedTarget;
}

export function sessionReference(session: SessionSummary): SessionReference {
  return {
    host_id:
      session.target.kind === "local" ? "local" : session.target.host_id!,
    session_id: session.session_id,
  };
}

export function workspaceTabKey(reference: WorkspaceTab): string {
  if (reference.kind === "task")
    return `task:${reference.host_id}:${reference.task_id}`;
  if (reference.kind === "task_definition")
    return `definition:${reference.definition_id}`;
  return JSON.stringify([
    reference.host_id === "local" ? "local" : `host:${reference.host_id}`,
    reference.session_id,
  ]);
}

export function restoreWorkspace(document: WorkspaceDocument): WorkspaceView {
  const ssh_gateways = document.ssh_gateways ?? [];
  const hosts = document.hosts.map(normalizeWorkspaceHost);
  const targets = hosts.map((host) => hostTarget(host, ssh_gateways));
  const targetsById = new Map(
    targets.map((target) => [
      target.kind === "local" ? "local" : target.host_id!,
      target,
    ]),
  );
  const shell_states = new Map<string, ShellStateSummary>();
  const sessions: SessionSummary[] = document.sessions.map((saved) => {
    const session: SessionSummary = {
      target: targetsById.get(saved.host_id)!,
      session_id: saved.session_id,
      name: saved.name,
      status: "unknown",
      next_sequence: "0",
      terminal_size: {
        columns: 80,
        rows: 24,
        pixel_width: null,
        pixel_height: null,
      },
    };
    if (saved.last_known_cwd) {
      shell_states.set(sessionKey(session), {
        shell_type: "unknown",
        cwd: saved.last_known_cwd,
        cwd_display: saved.last_known_cwd_display,
        running_command: null,
        prompt_phase: "unknown",
        tui_hint: "unknown",
        revision: "0",
        observed_sequence: "0",
      });
    }
    return session;
  });
  const byKey = new Map(
    sessions.map((session) => [sessionKey(session), session]),
  );
  return {
    targets,
    hosts,
    ssh_gateways,
    port_forwards: document.port_forwards ?? [],
    sessions,
    shell_states,
    sidebar_view:
      document.sidebar_view ??
      (document.active_tab?.kind === "task_definition" ? "tasks" : "sessions"),
    task_drafts: document.task_drafts ?? [],
    task_definitions: document.task_definitions ?? [],
    task_definition_scope: document.task_definition_scope ?? { kind: "global" },
    task_references: document.task_references ?? [],
    task_tabs: document.tabs.filter(
      (tab): tab is TaskTab => tab.kind === "task",
    ),
    tab_order: document.tabs
      .filter((tab) => tab.kind !== "task_definition")
      .map(workspaceTabKey),
    tabs: document.tabs
      .filter(
        (tab): tab is SessionReference => !tab.kind || tab.kind === "session",
      )
      .map((reference) => byKey.get(workspaceTabKey(reference))!)
      .filter(Boolean),
    active_tab_key:
      document.active_tab?.kind === "task_definition"
        ? (document.tabs
            .filter((tab) => tab.kind !== "task_definition")
            .map(workspaceTabKey)[0] ?? null)
        : document.active_tab
          ? workspaceTabKey(document.active_tab)
          : null,
  };
}

/** Deliberate allowlist: no terminal bytes, live status, command line, or secrets. */
export function workspaceDocument(
  view: WorkspaceView,
  workspace_id = "default",
): WorkspaceDocument & { hosts: WorkspaceHost[] } {
  const hostKeys = new Set(view.targets.map(targetKey));
  const sessions = view.sessions.filter((session) =>
    hostKeys.has(targetKey(session.target)),
  );
  const sessionKeys = new Set(sessions.map(sessionKey));
  const tabs = view.tabs.filter((tab) => sessionKeys.has(sessionKey(tab)));
  const allTabs: WorkspaceTab[] = [
    ...tabs.map((tab) => ({
      ...sessionReference(tab),
      kind: "session" as const,
    })),
    ...view.task_tabs.filter((tab) =>
      view.task_references.some(
        (item) => item.host_id === tab.host_id && item.task_id === tab.task_id,
      ),
    ),
  ];
  allTabs.sort((left, right) => {
    const a = view.tab_order.indexOf(workspaceTabKey(left));
    const b = view.tab_order.indexOf(workspaceTabKey(right));
    return (a < 0 ? Infinity : a) - (b < 0 ? Infinity : b);
  });
  const active = allTabs.find(
    (tab) => workspaceTabKey(tab) === view.active_tab_key,
  );
  return {
    schema_version: 7,
    ssh_gateways: view.ssh_gateways,
    port_forwards: view.port_forwards.filter((forward) =>
      view.targets.some(
        (target) => target.kind === "ssh" && target.host_id === forward.host_id,
      ),
    ),
    sidebar_view: view.sidebar_view,
    task_drafts: view.task_drafts,
    task_definition_scope: view.task_definition_scope,
    task_references: view.task_references,
    workspace_id,
    hosts: view.targets.map((target) => {
      const host_id = target.kind === "local" ? "local" : target.host_id;
      const host = view.hosts.find((known) => known.host_id === host_id) ?? hostFromTarget(target);
      return {
        ...host,
        connection_methods: host.connection_methods.map((method) => ({
          ...method, target: connectionSettings(method.target),
        })),
      };
    }),
    sessions: sessions.map((session) => {
      const shell = view.shell_states.get(sessionKey(session));
      return {
        ...sessionReference(session),
        name: session.name,
        last_known_cwd: shell?.cwd ?? null,
        last_known_cwd_display: shell?.cwd_display ?? null,
      };
    }),
    tabs: allTabs,
    active_tab: active ?? null,
  };
}
