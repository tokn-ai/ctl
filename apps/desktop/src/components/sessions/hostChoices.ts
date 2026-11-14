import { translate, type Locale } from "../../features/i18n/messages";
import type { ConnectionTarget, TailscaleDevice, WorkspaceHost } from "../../lib/types";
import { targetKey, targetLabel } from "../../features/targets/targets";

export function tailscaleDeviceDetail(device: TailscaleDevice, locale: Locale = "en"): string {
  return [
    translate(locale, device.online === null ? "host_group.unknown" : device.online ? "host_group.online" : "host_group.offline"),
    device.os,
    device.dns_name || device.addresses[0],
  ].filter(Boolean).join(" · ");
}

/** Group by ownership, so customized projections appear with saved hosts. */
export function hostSelectorChoices(
  targets: readonly ConnectionTarget[],
  hosts: readonly WorkspaceHost[] = [],
  localLabel?: string,
  locale: Locale = "en",
) {
  const hostRecords = new Map(hosts.map((host) => [host.host_id, host]));
  const groups = new Map<string, { id: string; label: string; group: string; detail?: string }[]>([
    ["host_group.machine", []],
    ["host_group.saved", []],
    ["host_group.virtual_ssh", []],
    ["host_group.virtual_tailscale", []],
    ["host_group.unavailable", []],
  ]);
  for (const target of targets) {
    const host = target.kind === "ssh" && target.host_id ? hostRecords.get(target.host_id) : undefined;
    const source = host?.source;
    if (source === "tailscale" && host?.tailscale_device?.online !== true) continue;
    const group = target.kind === "local" ? "host_group.machine"
      : source === "ssh_config" ? "host_group.virtual_ssh"
      : source === "tailscale" ? "host_group.virtual_tailscale"
      : source === "unavailable" ? "host_group.unavailable" : "host_group.saved";
    groups.get(group)!.push({
      id: targetKey(target),
      label: target.kind === "local" && localLabel !== undefined ? localLabel : targetLabel(target, locale),
      group: translate(locale, group),
      ...(host?.tailscale_device ? { detail: tailscaleDeviceDetail(host.tailscale_device, locale) } : {}),
    });
  }
  return [...groups.values()].flat();
}
