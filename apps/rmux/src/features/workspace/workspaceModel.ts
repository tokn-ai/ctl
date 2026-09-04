import type {
  WorkspaceTab,
  TaskTab,
  SavedTaskDefinition,
  TaskReference,
  ConnectionTarget,
  SessionReference,
  SessionSummary,
  ShellStateSummary,
  WorkspaceDocument,
} from "../../lib/types";
import { LOCAL_TARGET, sessionKey, targetKey } from "../targets/targets";

export interface WorkspaceView {
  task_definitions: SavedTaskDefinition[];
  task_references: TaskReference[];
  task_tabs: TaskTab[];
  tab_order: string[];
  targets: ConnectionTarget[];
  sessions: SessionSummary[];
  tabs: SessionSummary[];
  active_tab_key: string | null;
  shell_states: ReadonlyMap<string, ShellStateSummary>;
}

export function emptyWorkspaceView(): WorkspaceView {
  return {
    task_definitions: [], task_references: [], task_tabs: [], tab_order: [],
    targets: [LOCAL_TARGET],
    sessions: [],
    tabs: [],
    active_tab_key: null,
    shell_states: new Map(),
  };
}

export function withHostId(target: ConnectionTarget): ConnectionTarget {
  if (target.kind === "local") return LOCAL_TARGET;
  return { ...target, host_id: target.host_id ?? crypto.randomUUID() };
}

export function sessionReference(session: SessionSummary): SessionReference {
  return {
    host_id:
      session.target.kind === "local" ? "local" : session.target.host_id!,
    session_id: session.session_id,
  };
}

export function workspaceTabKey(reference: WorkspaceTab): string {
  if (reference.kind === "task") return `task:${reference.host_id}:${reference.task_id}`;
  if (reference.kind === "task_definition") return `definition:${reference.definition_id}`;
  return JSON.stringify([
    reference.host_id === "local" ? "local" : `host:${reference.host_id}`,
    reference.session_id,
  ]);
}

export function restoreWorkspace(document: WorkspaceDocument): WorkspaceView {
  const targets = document.hosts.map(({ host_id, target }) =>
    target.kind === "local" ? LOCAL_TARGET : { ...target, host_id },
  );
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
    sessions,
    shell_states,
    task_definitions: document.task_definitions ?? [],
    task_references: document.task_references ?? [],
    task_tabs: document.tabs.filter((tab): tab is TaskTab => tab.kind === "task" || tab.kind === "task_definition"),
    tab_order: document.tabs.map(workspaceTabKey),
    tabs: document.tabs.filter((tab): tab is SessionReference => !tab.kind || tab.kind === "session").map((reference) => byKey.get(workspaceTabKey(reference))!).filter(Boolean),
    active_tab_key: document.active_tab
      ? workspaceTabKey(document.active_tab)
      : null,
  };
}

/** Deliberate allowlist: no terminal bytes, live status, command line, or secrets. */
export function workspaceDocument(
  view: WorkspaceView,
  workspace_id = "default",
): WorkspaceDocument {
  const hostKeys = new Set(view.targets.map(targetKey));
  const sessions = view.sessions.filter((session) =>
    hostKeys.has(targetKey(session.target)),
  );
  const sessionKeys = new Set(sessions.map(sessionKey));
  const tabs = view.tabs.filter((tab) => sessionKeys.has(sessionKey(tab)));
  const allTabs: WorkspaceTab[] = [...tabs.map((tab) => ({ ...sessionReference(tab), kind: "session" as const })), ...view.task_tabs.filter((tab) => tab.kind === "task_definition" ? view.task_definitions.some((item) => item.definition_id === tab.definition_id) : view.task_references.some((item) => item.host_id === tab.host_id && item.task_id === tab.task_id))];
  allTabs.sort((left, right) => {
    const a = view.tab_order.indexOf(workspaceTabKey(left));
    const b = view.tab_order.indexOf(workspaceTabKey(right));
    return (a < 0 ? Infinity : a) - (b < 0 ? Infinity : b);
  });
  const active = allTabs.find((tab) => workspaceTabKey(tab) === view.active_tab_key);
  return {
    schema_version: 2,
    task_definitions: view.task_definitions,
    task_references: view.task_references.map((item) => item.definition_id && !view.task_definitions.some((definition) => definition.definition_id === item.definition_id) ? { ...item, definition_id: null, applied_revision: null, is_default: false } : item),
    workspace_id,
    hosts: view.targets.map((target) => {
      if (target.kind === "local")
        return { host_id: "local", target: LOCAL_TARGET };
      const { host_id, ...connection } = target;
      return { host_id: host_id!, target: connection };
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
