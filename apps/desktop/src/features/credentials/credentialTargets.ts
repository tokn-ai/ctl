import type { CredentialTarget, WorkspaceHost, WorkspaceSshGateway } from "../../lib/types";
import { hostTarget } from "../workspace/workspaceModel";

/** Keep file hints for unavailable hosts; only available routes match legacy credential scopes. */
export function credentialTargets(
  hosts: readonly WorkspaceHost[],
  gateways: readonly WorkspaceSshGateway[],
): CredentialTarget[] {
  return hosts.flatMap((host) => host.connection_methods.flatMap((method) => {
    const target = hostTarget(host, gateways, method.method_id);
    if (target.kind !== "ssh") return [];
    if (!target.unavailable) return [{ name: host.name, target }];
    // Unavailable hosts bypass route resolution. Preserve known gateway key
    // paths for inventory without making the route connectable.
    const resolved = (target.gateway_route ?? []).flatMap((step) => {
      const gateway = gateways.find((candidate) => candidate.gateway_id === step.gateway_id);
      return gateway ? [{ ...gateway, mode: step.mode }] : [];
    });
    return [{ name: host.name, target: { ...target, gateways: resolved } }];
  }));
}
