import type { ConnectionTarget, WorkspaceHost } from "../../lib/types";
import { targetKey, targetLabel } from "../../features/targets/targets";

export const VIRTUAL_SSH_GROUP = "SSH config · Virtual";

/** Group by ownership, so customized SSH aliases appear with saved hosts. */
export function hostSelectorChoices(
  targets: readonly ConnectionTarget[],
  hosts: readonly WorkspaceHost[] = [],
  localLabel = "local",
) {
  const sources = new Map(hosts.map((host) => [host.host_id, host.source]));
  const groups = new Map<string, { id: string; label: string; group: string }[]>([
    ["This machine", []],
    ["Saved hosts", []],
    [VIRTUAL_SSH_GROUP, []],
    ["Unavailable hosts", []],
  ]);
  for (const target of targets) {
    const source = target.kind === "ssh" && target.host_id ? sources.get(target.host_id) : undefined;
    const group = target.kind === "local" ? "This machine"
      : source === "ssh_config" ? VIRTUAL_SSH_GROUP
      : source === "unavailable" ? "Unavailable hosts" : "Saved hosts";
    groups.get(group)!.push({
      id: targetKey(target),
      label: target.kind === "local" ? localLabel : targetLabel(target),
      group,
    });
  }
  return [...groups.values()].flat();
}
