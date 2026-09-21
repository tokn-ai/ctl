import type {
  ConnectionTarget,
  RemoteIdentity,
  SessionSummary,
  SshConnectionTarget,
} from "../../lib/types";
import { targetKey } from "../targets/targets";
import { connectionSettings, expectedHostIdentity, promoteHost, type WorkspaceView } from "./workspaceModel";

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
    left.identity_file === right.identity_file &&
    JSON.stringify(left.gateway_route ?? []) ===
      JSON.stringify(right.gateway_route ?? []) &&
    JSON.stringify(left.gateways ?? []) === JSON.stringify(right.gateways ?? [])
  );
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

/** Explicitly connect one host; verifying an address never merges hosts. */
export function recoverRemoteHost(
  view: WorkspaceView,
  candidate: SshConnectionTarget,
  remote_info: RemoteIdentity,
) {
  if (!candidate.host_id) return null;
  const host = view.hosts.find((item) => item.host_id === candidate.host_id);
  if (!host) throw new Error("This host is no longer in the workspace.");
  if (candidate.unavailable || host.source === "unavailable")
    throw new Error(candidate.unavailable ?? "This host is unavailable. Restore its definition before connecting.");
  const expected = expectedHostIdentity(host);
  if (expected && expected.remote_id !== remote_info.remote_id)
    throw new Error("This connection reaches a different remote environment. Each host supports one account; add a separate host for another account.");
  const method_id = candidate.method_id ?? host.preferred_method_id;
  const method = host.connection_methods.find((method) => method.method_id === method_id);
  if (!method)
    throw new Error("This connection method is no longer saved on the host.");
  if (method.target.unavailable) throw new Error(method.target.unavailable);
  const save_ssh_user = host.source === "tailscale" && Boolean(candidate.user?.trim()) &&
    candidate.user !== method.target.user;
  const recovered_host = {
    ...(save_ssh_user ? promoteHost(host) : host),
    remote_info,
    ...(host.expected_remote_info ? { expected_remote_info: remote_info } : {}),
    ...(save_ssh_user ? {
      connection_methods: host.connection_methods.map((item) => item.method_id === method_id ? {
        ...item,
        target: connectionSettings(candidate),
      } : item),
    } : {}),
  };
  const target: SshConnectionTarget = {
    ...candidate,
    host_name: host.name,
    method_id: method_id!,
    remote_info,
  };
  const remap = (session: SessionSummary): SessionSummary =>
    targetKey(session.target) === targetKey(target) ? { ...session, target } : session;
  return {
    target,
    key_changes: new Map<string, string>(),
    view: {
      ...view,
      hosts: view.hosts.map((item) => item.host_id === host.host_id ? recovered_host : item),
      targets: view.targets.map((item) => targetKey(item) === targetKey(target) ? target : item),
      sessions: view.sessions.map(remap),
      tabs: view.tabs.map(remap),
    } satisfies WorkspaceView,
  };
}
