import type { ConnectionTarget, SshConnectionTarget } from "../../lib/types";
import { hostTarget, type WorkspaceView } from "./workspaceModel";

/** Match ctld/keychain.rs: a destination plus the effective ordered gateway chain. */
function credentialScope(target: SshConnectionTarget): string {
  return JSON.stringify([
    target.destination,
    (target.gateways ?? []).map((gateway) => ({
      kind: gateway.kind ?? "ssh",
      destination: gateway.destination,
      hostname: gateway.hostname ?? null,
      user: gateway.user ?? null,
      port: gateway.port ?? null,
      identity_file: gateway.identity_file ?? null,
      mode: gateway.mode,
    })),
  ]);
}

/** Delete only scopes that no surviving saved method or live snapshot still uses. */
export function removableHostCredentials(
  view: WorkspaceView,
  host_id: string,
  attachment_target?: ConnectionTarget,
): SshConnectionTarget[] {
  const targets = [
    ...view.hosts.flatMap((host) => host.connection_methods.map((method) =>
      hostTarget(host, view.ssh_gateways, method.method_id))),
    ...view.targets,
    ...view.sessions.map((session) => session.target),
    ...view.tabs.map((session) => session.target),
    ...(attachment_target ? [attachment_target] : []),
  ].filter((target): target is SshConnectionTarget => target.kind === "ssh" && !target.unavailable);
  const shared = new Set(targets
    .filter((target) => target.host_id !== host_id)
    .map(credentialScope));
  const removable = new Map<string, SshConnectionTarget>();
  for (const target of targets) {
    if (target.host_id !== host_id) continue;
    const scope = credentialScope(target);
    if (!shared.has(scope) && !removable.has(scope)) removable.set(scope, target);
  }
  return [...removable.values()];
}
