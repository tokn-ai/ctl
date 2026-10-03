import type { CredentialTarget, ResolvedSshGateway, WorkspaceHost, WorkspaceSshGateway } from "../../lib/types";
import { hostTarget, resolveSshGateways } from "../workspace/workspaceModel";
import { isHostRouteStep, isVpnRouteStep, orderedSshRoute, resolveVpnRouteStep } from "../workspace/sshRoute";

/** Keep file hints for unavailable hosts; only available routes match legacy credential scopes. */
export function credentialTargets(
  hosts: readonly WorkspaceHost[],
  gateways: readonly WorkspaceSshGateway[],
): CredentialTarget[] {
  return hosts.flatMap((host) => host.connection_methods.flatMap((method) => {
    const target = hostTarget(host, gateways, method.method_id, hosts);
    if (target.kind !== "ssh") return [];
    if (!target.unavailable) return [{ name: host.name, target }];
    // Unavailable hosts bypass route resolution. Preserve known gateway key
    // paths for inventory without making the route connectable.
    try {
      return [{ name: host.name, target: resolveSshGateways(target, gateways, hosts) }];
    } catch {
      // Partial inventory is useful only as file hints, never as a usable scope.
    }
    const active = new Set([host.host_id]);
    const hints = (route: ReturnType<typeof orderedSshRoute>): ResolvedSshGateway[] => route.flatMap((step): ResolvedSshGateway[] => {
      if (isVpnRouteStep(step)) return [resolveVpnRouteStep(step)];
      if (isHostRouteStep(step)) {
        const linked = hosts.find((candidate) => candidate.host_id === step.host_id);
        const selected = linked?.connection_methods.find((candidate) => candidate.method_id === step.method_id);
        if (!linked || !selected || active.has(linked.host_id)) return [];
        active.add(linked.host_id);
        const prefix = hints(orderedSshRoute(selected.target));
        active.delete(linked.host_id);
        return [...prefix, { gateway_id: `host:${linked.host_id}:${selected.method_id}`, name: linked.name,
          destination: selected.ssh_config_alias ?? selected.target.destination,
          ...(selected.target.identity_file ? { identity_file: selected.target.identity_file } : {}), mode: step.mode }];
      }
      const gateway = gateways.find((candidate) => candidate.gateway_id === step.gateway_id);
      return gateway ? [{ ...gateway, mode: step.mode }] : [];
    });
    const resolved = hints(target.gateway_route ?? []);
    return [{ name: host.name, target: { ...target, gateways: resolved } }];
  }));
}
