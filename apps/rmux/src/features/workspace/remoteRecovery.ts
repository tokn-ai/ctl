import type {
  ConnectionTarget,
  RemoteIdentity,
  SessionSummary,
  SshConnectionTarget,
} from "../../lib/types";
import { sessionKey, targetKey } from "../targets/targets";
import { workspaceTabKey, type WorkspaceView } from "./workspaceModel";

export function sameSshEndpoint(
  left: ConnectionTarget,
  right: ConnectionTarget,
): boolean {
  if (left.kind !== "ssh" || right.kind !== "ssh")
    return left.kind === right.kind;
  return (
    left.destination === right.destination &&
    left.hostname === right.hostname &&
    left.user === right.user &&
    left.port === right.port &&
    left.identity_file === right.identity_file
  );
}

function unique<T>(items: T[], key: (item: T) => string): T[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    const value = key(item);
    if (seen.has(value)) return false;
    seen.add(value);
    return true;
  });
}

export function remapStateKeys<T>(
  current: ReadonlyMap<string, T>,
  key_changes: ReadonlyMap<string, string>,
): Map<string, T> {
  const next = new Map(current);
  for (const [old_key, new_key] of key_changes) {
    if (old_key === new_key) continue;
    const state = next.get(old_key);
    next.delete(old_key);
    if (state !== undefined && !next.has(new_key)) next.set(new_key, state);
  }
  return next;
}

/** Rebind one verified environment atomically, retaining local IDs and tab state. */
export function recoverRemoteHost(
  view: WorkspaceView,
  candidate: SshConnectionTarget,
  remote_info: RemoteIdentity,
) {
  const hosts = view.targets.filter(
    (target): target is SshConnectionTarget => target.kind === "ssh",
  );
  const addressed = hosts.find((host) =>
    candidate.host_id
      ? host.host_id === candidate.host_id
      : host.destination === candidate.destination,
  );
  const collision = hosts.find(
    (host) => host.destination === candidate.destination,
  );
  for (const host of [addressed, collision]) {
    if (host?.remote_info && host.remote_info.remote_id !== remote_info.remote_id)
      throw new Error(
        "This address belongs to a different saved remote environment. Choose a different host name.",
      );
  }
  const known = hosts.filter(
    (host) => host.remote_info?.remote_id === remote_info.remote_id,
  );
  const canonical = known[0] ?? addressed;
  if (!canonical) return null;
  const affected = new Set(
    [canonical, ...known, addressed, collision]
      .filter((host) => host !== undefined)
      .map((host) => host.host_id!),
  );
  const target: SshConnectionTarget = {
    ...candidate,
    host_id: canonical.host_id!,
    remote_info,
  };
  const key_changes = new Map<string, string>();
  const remap = (session: SessionSummary): SessionSummary => {
    if (session.target.kind !== "ssh" || !affected.has(session.target.host_id!))
      return session;
    const next = { ...session, target };
    key_changes.set(sessionKey(session), sessionKey(next));
    return next;
  };
  const sessions = unique(view.sessions.map(remap), sessionKey);
  const tabs = unique(view.tabs.map(remap), sessionKey);
  const remapTask = <T extends { host_id: string; task_id: string }>(task: T): T => {
    if (!affected.has(task.host_id)) return task;
    const next = { ...task, host_id: target.host_id! };
    key_changes.set(
      workspaceTabKey({ ...task, kind: "task" }),
      workspaceTabKey({ ...next, kind: "task" }),
    );
    return next;
  };
  const taskKey = (task: { host_id: string; task_id: string }) =>
    JSON.stringify([task.host_id, task.task_id]);
  const task_tabs = unique(view.task_tabs.map(remapTask), taskKey);
  const defaults = new Set<string>();
  const task_references = unique(view.task_references.map(remapTask), taskKey).map(
    (task) => {
      if (!task.is_default || !task.definition_id) return task;
      const key = JSON.stringify([
        task.host_id,
        task.definition_scope ?? { kind: "global" },
        task.definition_id,
      ]);
      if (defaults.has(key)) return { ...task, is_default: false };
      defaults.add(key);
      return task;
    },
  );
  const shell_states = remapStateKeys(view.shell_states, key_changes);
  return {
    target,
    key_changes,
    view: {
      ...view,
      targets: view.targets.flatMap((host) => {
        if (targetKey(host) === targetKey(canonical)) return [target];
        return host.kind === "ssh" && affected.has(host.host_id!) ? [] : [host];
      }),
      sessions,
      tabs,
      task_tabs,
      task_references,
      shell_states,
      tab_order: [...new Set(view.tab_order.map((key) => key_changes.get(key) ?? key))],
      active_tab_key: view.active_tab_key
        ? key_changes.get(view.active_tab_key) ?? view.active_tab_key
        : null,
    } satisfies WorkspaceView,
  };
}
