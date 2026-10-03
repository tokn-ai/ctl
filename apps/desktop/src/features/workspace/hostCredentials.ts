import type { ConnectionTarget, SshConnectionTarget } from "../../lib/types";
import { hostTarget, type WorkspaceView } from "./workspaceModel";
import { resolveVpnRouteStep } from "./sshRoute";

/** Match ctld/keychain.rs: destination and route, including the stable VPN binding. */
function credentialScope(target: SshConnectionTarget): string {
  const route = [
    ...(target.vpn_connection_id ? [resolveVpnRouteStep({ vpn_connection_id: target.vpn_connection_id })] : []),
    ...(target.gateways ?? []),
  ];
  return JSON.stringify([
    target.destination,
    route.map((gateway, index) => ({
      kind: gateway.kind ?? "ssh",
      destination: gateway.destination,
      hostname: gateway.hostname ?? null,
      user: gateway.user ?? null,
      port: gateway.port ?? null,
      identity_file: gateway.identity_file ?? null,
      mode: gateway.mode,
      ...(gateway.kind === "vpn" ? {
        vpn_connection_id: gateway.vpn_connection_id,
        expected_remote_id: route[index - 1]?.kind !== "socks5" ? route[index - 1]?.remote_info?.remote_id ?? null : null,
      } : {}),
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
      hostTarget(host, view.ssh_gateways, method.method_id, view.hosts))),
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
