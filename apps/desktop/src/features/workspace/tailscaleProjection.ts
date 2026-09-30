import type {
  SshConnectionTarget,
  TailscaleDevice,
  WorkspaceConnectionMethod,
  WorkspaceHost,
} from "../../lib/types";

const TAILSCALE_PREFIX = "tailscale:";

export function tailscaleHostId(node_id: string): string {
  return `${TAILSCALE_PREFIX}${encodeURIComponent(node_id)}`;
}

export function tailscaleNodeId(host_id: string): string | null {
  if (!host_id.startsWith(TAILSCALE_PREFIX)) return null;
  try {
    return decodeURIComponent(host_id.slice(TAILSCALE_PREFIX.length)) || null;
  } catch {
    return null;
  }
}

export const TAILSCALE_UNAVAILABLE = "This Tailscale device is no longer available. Check that Tailscale is running and signed in to the correct tailnet, then refresh hosts.";

function isIpAddress(address: string): boolean {
  if (!address.includes(":")) {
    const parts = address.split(".");
    return parts.length === 4 && parts.every((part) => /^\d{1,3}$/u.test(part) && Number(part) <= 255);
  }
  if (!/^[\da-f:.]+$/iu.test(address)) return false;
  try {
    new URL(`http://[${address}]/`);
    return true;
  } catch {
    return false;
  }
}

/** The binding follows a node across renames and address changes, never another machine at its old address. */
export function resolveTailscaleMethod(
  method: WorkspaceConnectionMethod,
  device: TailscaleDevice | undefined,
): WorkspaceConnectionMethod {
  const { unavailable: _unavailable, hostname: _hostname, ...target } = method.target;
  if (!device) return {
    ...method,
    target: { ...method.target, unavailable: TAILSCALE_UNAVAILABLE },
  };
  const hostname = device.addresses.find(isIpAddress);
  const destination = device.dns_name || hostname;
  return {
    ...method,
    target: {
      ...target,
      destination: destination ?? device.name,
      ...(hostname ? { hostname } : {}),
      ...(!destination ? { unavailable: "This Tailscale device has no usable address. Check its Tailscale connection, then refresh hosts." } : {}),
    },
  };
}

export function projectedTailscaleHost(device: TailscaleDevice): WorkspaceHost {
  const target: SshConnectionTarget = { kind: "ssh", destination: device.name };
  return {
    host_id: tailscaleHostId(device.node_id),
    name: device.name,
    source: "tailscale",
    tailscale_device: device,
    preferred_method_id: "tailscale",
    connection_methods: [resolveTailscaleMethod({
      method_id: "tailscale",
      name: "Tailscale",
      tailscale_node_id: device.node_id,
      target,
    }, device)],
  };
}

export function tailscaleTarget(device: TailscaleDevice): SshConnectionTarget {
  const host = projectedTailscaleHost(device);
  return {
    ...host.connection_methods[0].target,
    host_id: host.host_id,
    host_name: host.name,
    method_id: "tailscale",
    tailscale_node_id: device.node_id,
  };
}
