import type { ConnectionTarget, TailscaleDevice, WorkspaceHost } from "../../lib/types";
import { targetKey, targetLabel } from "../../features/targets/targets";

export const VIRTUAL_SSH_GROUP = "SSH config · Virtual";
export const VIRTUAL_TAILSCALE_GROUP = "Tailscale · Virtual";

export function tailscaleDeviceDetail(device: TailscaleDevice): string {
  return [
    device.online === null ? "Status unknown" : device.online ? "Online" : "Offline",
    device.os,
    device.dns_name || device.addresses[0],
  ].filter(Boolean).join(" · ");
}

/** Group by ownership, so customized projections appear with saved hosts. */
export function hostSelectorChoices(
  targets: readonly ConnectionTarget[],
  hosts: readonly WorkspaceHost[] = [],
  localLabel = "local",
) {
  const hostRecords = new Map(hosts.map((host) => [host.host_id, host]));
  const groups = new Map<string, { id: string; label: string; group: string; detail?: string }[]>([
    ["This machine", []],
    ["Saved hosts", []],
    [VIRTUAL_SSH_GROUP, []],
    [VIRTUAL_TAILSCALE_GROUP, []],
    ["Unavailable hosts", []],
  ]);
  for (const target of targets) {
    const host = target.kind === "ssh" && target.host_id ? hostRecords.get(target.host_id) : undefined;
    const source = host?.source;
    if (source === "tailscale" && host?.tailscale_device?.online !== true) continue;
    const group = target.kind === "local" ? "This machine"
      : source === "ssh_config" ? VIRTUAL_SSH_GROUP
      : source === "tailscale" ? VIRTUAL_TAILSCALE_GROUP
      : source === "unavailable" ? "Unavailable hosts" : "Saved hosts";
    groups.get(group)!.push({
      id: targetKey(target),
      label: target.kind === "local" ? localLabel : targetLabel(target),
      group,
      ...(host?.tailscale_device ? { detail: tailscaleDeviceDetail(host.tailscale_device) } : {}),
    });
  }
  return [...groups.values()].flat();
}
