import type {
  ConnectionTarget,
  SessionReference,
  SessionSummary,
  ShellStateSummary,
  WorkspaceDocument,
} from "../../lib/types";
import { LOCAL_TARGET, sessionKey, targetKey } from "../targets/targets";

export interface WorkspaceView {
  targets: ConnectionTarget[];
  sessions: SessionSummary[];
  tabs: SessionSummary[];
  active_tab_key: string | null;
  shell_states: ReadonlyMap<string, ShellStateSummary>;
}

export function emptyWorkspaceView(): WorkspaceView {
  return {
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

function referenceKey(reference: SessionReference): string {
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
    tabs: document.tabs.map((reference) => byKey.get(referenceKey(reference))!),
    active_tab_key: document.active_tab
      ? referenceKey(document.active_tab)
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
  const active = tabs.find((tab) => sessionKey(tab) === view.active_tab_key);
  return {
    schema_version: 1,
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
    tabs: tabs.map(sessionReference),
    active_tab: active ? sessionReference(active) : null,
  };
}
