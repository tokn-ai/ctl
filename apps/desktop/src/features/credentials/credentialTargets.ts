import type { CredentialTarget, WorkspaceHost, WorkspaceSshGateway } from "../../lib/types";
import { hostTarget } from "../workspace/workspaceModel";

/** Names are hints for legacy items; the native helper matches exact credential scopes. */
export function credentialTargets(
  hosts: readonly WorkspaceHost[],
  gateways: readonly WorkspaceSshGateway[],
): CredentialTarget[] {
  return hosts.flatMap((host) => host.connection_methods.flatMap((method) => {
    const target = hostTarget(host, gateways, method.method_id);
    return target.kind === "ssh" && !target.unavailable
      ? [{ name: host.name, target }]
      : [];
  }));
}
