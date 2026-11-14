import { LocalizedError, message } from "../i18n/messages";
import type {
  ResolvedVpnGateway,
  ResolvedSshGateway,
  HostGatewayReference,
  SshConnectionTarget,
  SshGatewayRouteStep,
  VpnGatewayReference,
  WorkspaceSshGateway,
  WorkspaceHost,
} from "../../lib/types";

export function isVpnRouteStep(step: SshGatewayRouteStep): step is VpnGatewayReference {
  return "vpn_connection_id" in step;
}

export function isHostRouteStep(step: SshGatewayRouteStep): step is HostGatewayReference {
  return "host_id" in step;
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

/** Resolve live catalog links without changing the persisted route references. */
export function expandSshRoute(
  target: SshConnectionTarget,
  gateways: readonly WorkspaceSshGateway[],
  hosts: readonly WorkspaceHost[] = [],
): ResolvedSshGateway[] {
  const gateway_by_id = new Map(gateways.map((gateway) => [gateway.gateway_id, gateway]));
  const host_by_id = new Map(hosts.map((host) => [host.host_id, host]));
  const resolved: ResolvedSshGateway[] = [];
  const active_hosts = new Set(target.host_id ? [target.host_id] : []);
  const append = (gateway: ResolvedSshGateway) => {
    if (resolved.length >= 8) throw new LocalizedError("workspace.route_too_long");
    const preceding = resolved[resolved.length - 1];
    if (gateway.kind === "vpn" && resolved.length &&
      (preceding?.kind === "vpn" || preceding?.kind === "socks5")) {
      throw new LocalizedError("workspace.route_vpn_position");
    }
    resolved.push(gateway);
  };
  const expand = (route: readonly SshGatewayRouteStep[], depth = 0) => {
    for (const step of route) {
      if (isVpnRouteStep(step)) {
        if (!step.vpn_connection_id?.trim()) throw new LocalizedError("workspace.route_vpn_missing");
        append(resolveVpnRouteStep(step));
      } else if (isHostRouteStep(step)) {
        const host = host_by_id.get(step.host_id);
        if (!host || host.host_id === "local") throw new LocalizedError("workspace.route_host_missing");
        if (active_hosts.has(host.host_id)) throw new LocalizedError("workspace.route_cycle", { host: host.name });
        if (depth >= 8) throw new LocalizedError("workspace.route_too_long");
        const method = host.connection_methods.find((candidate) => candidate.method_id === step.method_id);
        if (!method) throw new LocalizedError("workspace.route_method_missing", { host: host.name });
        if (host.source === "unavailable" || method.target.unavailable) {
          throw new LocalizedError("workspace.route_hop_unavailable", { host: host.name, detail: method.target.unavailable ?? message("workspace.route_restore_hop") });
        }
        if (method.target.identity_file) {
          throw new LocalizedError("workspace.route_identity_file", { method: method.name, host: host.name });
        }
        active_hosts.add(host.host_id);
        expand(orderedSshRoute(method.target), depth + 1);
        active_hosts.delete(host.host_id);
        const endpoint = method.target;
        const address = method.ssh_config_alias ?? endpoint.destination;
        const separator = address.lastIndexOf("@");
        const destination = separator < 0 ? address : address.slice(separator + 1);
        const user = endpoint.user ?? (separator < 0 ? undefined : address.slice(0, separator));
        if (!destination || (separator >= 0 && (!user || user.includes("@")))) {
          throw new LocalizedError("workspace.route_address", { host: host.name });
        }
        const remote_info = host.expected_remote_info ?? host.remote_info;
        append({
          kind: "ssh",
          gateway_id: `host:${host.host_id}:${method.method_id}`,
          name: host.name,
          destination,
          ...(endpoint.hostname ? { hostname: endpoint.hostname } : {}),
          ...(user ? { user } : {}),
          ...(endpoint.port ? { port: endpoint.port } : {}),
          ...(remote_info ? { remote_info } : {}),
          mode: step.mode,
        });
      } else {
        const gateway = gateway_by_id.get(step.gateway_id);
        if (!gateway) throw new LocalizedError("workspace.route_gateway_missing");
        append({ ...gateway, mode: step.mode });
      }
    }
  };
  expand(orderedSshRoute(target));
  return resolved;
}

/** Runtime snapshots already contain the complete route, including inherited hops. */
export function resolvedVpnExecutionTarget(
  route: readonly ResolvedSshGateway[],
  index: number,
): SshConnectionTarget | undefined {
  if (index <= 0 || route[index]?.kind !== "vpn") return undefined;
  const owner = route[index - 1];
  if (!owner || owner.kind === "vpn" || owner.kind === "socks5") return undefined;
  const prefix = route.slice(0, index - 1);
  return { kind: "ssh", destination: owner.destination,
    ...(owner.hostname ? { hostname: owner.hostname, ssh_config_alias: owner.destination } : {}),
    ...(owner.user ? { user: owner.user } : {}),
    ...(owner.port ? { port: owner.port } : {}),
    ...(owner.identity_file ? { identity_file: owner.identity_file } : {}),
    ...(owner.remote_info ? { remote_info: owner.remote_info } : {}),
    use_ssh_config_master: false,
    ...(prefix.length ? { gateways: prefix } : {}) };
}

/** Connect to the SSH host immediately before this VPN, through its route prefix. */
export function vpnExecutionTarget(
  route: readonly SshGatewayRouteStep[],
  index: number,
  gateways: readonly WorkspaceSshGateway[],
  hosts: readonly WorkspaceHost[] = [],
  destination_host_id?: string,
): SshConnectionTarget | undefined {
  if (index === 0) return undefined;
  if (!route[index] || !isVpnRouteStep(route[index])) return undefined;
  const resolved = expandSshRoute({ kind: "ssh", destination: "VPN execution host", host_id: destination_host_id,
    gateway_route: route.slice(0, index + 1) }, gateways, hosts);
  const owner = resolvedVpnExecutionTarget(resolved, resolved.length - 1);
  if (!owner) return undefined;
  // Preserve legacy source prefixes where they completely describe the owner.
  const prefix = route.slice(0, index - 1);
  return !isHostRouteStep(route[index - 1]) && prefix.length
    ? { ...owner, gateway_route: prefix } : owner;
}
