import type { ConnectionTarget, SshConnectionTarget } from "../../lib/types";
import { errorCode } from "../../lib/errors";
import { resolveVpnRouteStep, resolvedVpnExecutionTarget } from "./sshRoute";

/** Use the failed attempt's transport snapshot to update exactly its VPN owner. */
export function remoteVpnUpdateOwner(
  target: ConnectionTarget,
  error: unknown,
): { target: SshConnectionTarget; name: string } | null {
  if (target.kind !== "ssh" || errorCode(error) !== "remote_vpn_components_update_required" ||
    typeof error !== "object" || error === null || !("vpn_route_index" in error)) return null;
  const vpn_route_index = error.vpn_route_index;
  if (typeof vpn_route_index !== "number" || !Number.isInteger(vpn_route_index) || vpn_route_index < 0) return null;
  const route = [
    ...(target.vpn_connection_id ? [resolveVpnRouteStep({ vpn_connection_id: target.vpn_connection_id })] : []),
    ...(target.gateways ?? []),
  ];
  const index = route.flatMap((gateway, index) => gateway.kind === "vpn" ? [index] : [])[vpn_route_index];
  if (index === undefined) return null;
  const owner = resolvedVpnExecutionTarget(route, index);
  return owner ? { target: owner, name: route[index - 1].name } : null;
}
