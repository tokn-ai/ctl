import type {
  ResolvedVpnGateway,
  ResolvedSshGateway,
  SshConnectionTarget,
  SshGatewayRouteStep,
  VpnGatewayReference,
  WorkspaceSshGateway,
} from "../../lib/types";

export function isVpnRouteStep(step: SshGatewayRouteStep): step is VpnGatewayReference {
  return "vpn_connection_id" in step;
}

/** The legacy VPN field always runs locally, before the ordered route. */
export function orderedSshRoute(target: SshConnectionTarget): SshGatewayRouteStep[] {
  return [
    ...(target.vpn_connection_id ? [{ vpn_connection_id: target.vpn_connection_id }] : []),
    ...(target.gateway_route ?? []),
  ];
}

/** Local status and local sign-in must never describe a VPN running on a jump host. */
export function localVpnConnectionId(target: SshConnectionTarget): string | undefined {
  if (target.vpn_connection_id) return target.vpn_connection_id;
  const first = target.gateway_route?.[0];
  if (first && isVpnRouteStep(first)) return first.vpn_connection_id;
  const runtime_first = target.gateways?.[0];
  return runtime_first?.kind === "vpn" ? runtime_first.vpn_connection_id : undefined;
}

export function hasVpnRoute(target: SshConnectionTarget): boolean {
  return Boolean(target.vpn_connection_id) ||
    Boolean(target.gateway_route?.some(isVpnRouteStep)) ||
    Boolean(target.gateways?.some((gateway) => gateway.kind === "vpn"));
}

export function resolveVpnRouteStep(step: VpnGatewayReference): ResolvedVpnGateway {
  return {
    kind: "vpn",
    gateway_id: `vpn:${step.vpn_connection_id}`,
    name: step.vpn_connection_id,
    destination: step.vpn_connection_id,
    vpn_connection_id: step.vpn_connection_id,
    mode: "automatic",
  };
}

/** Connect to the SSH host immediately before this VPN, through its route prefix. */
export function vpnExecutionTarget(
  route: readonly SshGatewayRouteStep[],
  index: number,
  gateways: readonly WorkspaceSshGateway[],
): SshConnectionTarget | undefined {
  if (index === 0) return undefined;
  const preceding = route[index - 1];
  if (!preceding || isVpnRouteStep(preceding)) return undefined;
  const owner = gateways.find((gateway) => gateway.gateway_id === preceding.gateway_id);
  if (!owner || owner.kind === "socks5") return undefined;
  const { gateway_id: _id, name: _name, kind: _kind, ...endpoint } = owner;
  const prefix = route.slice(0, index - 1);
  const resolved: ResolvedSshGateway[] = [];
  for (const step of prefix) {
    if (isVpnRouteStep(step)) {
      resolved.push(resolveVpnRouteStep(step));
      continue;
    }
    const gateway = gateways.find((item) => item.gateway_id === step.gateway_id);
    if (!gateway) return undefined;
    resolved.push({ ...gateway, mode: step.mode });
  }
  return {
    kind: "ssh",
    ...endpoint,
    ...(prefix.length ? { gateway_route: prefix, gateways: resolved } : {}),
  };
}
