import { useEffect, useRef } from "react";
import type { ConnectionTarget, HostConnectionStatus } from "../../lib/types";
import { COMMAND_IDS } from "../commands/commandIds";
import { targetKey, targetLabel } from "../targets/targets";
import type { NotificationStore } from "./NotificationStore";

/** Observe proven connection loss; terminal attachments own their own recovery. */
export function useHostConnectionNotifications(
  store: NotificationStore | undefined,
  targets: readonly ConnectionTarget[],
  statuses: ReadonlyMap<string, HostConnectionStatus>,
  attached_host_keys: ReadonlySet<string>,
) {
  const available_hosts = useRef(new Set<string>());
  const lost_hosts = useRef(new Set<string>());
  useEffect(() => {
    const existing_hosts = new Set<string>();
    for (const target of targets) {
      if (target.kind !== "ssh" || !target.host_id) continue;
      const host_id = target.host_id;
      existing_hosts.add(host_id);
      const status = statuses.get(host_id);
      const observation = status?.observation;
      if (status?.manually_disconnected || status?.operation?.state === "pending" || status?.operation?.state === "failed") {
        available_hosts.current.delete(host_id);
        continue;
      }
      if (!observation || observation.stale || observation.completeness !== "complete") continue;
      const source_key = `host-connection:${host_id}`;
      if (observation.availability === "available") {
        available_hosts.current.add(host_id);
        if (lost_hosts.current.delete(host_id)) store?.resolve(source_key);
      } else if (observation.availability === "unavailable" && available_hosts.current.delete(host_id)) {
        if (lost_hosts.current.has(host_id)) continue;
        lost_hosts.current.add(host_id);
        if (attached_host_keys.has(host_id)) continue;
        store?.report(source_key, {
          severity: "warning", title: "SSH connection lost", source: targetLabel(target),
          message: observation.message ?? "The SSH connection is no longer open. Connect this host to resume.",
          actions: [{ label: "Connect host", command_id: COMMAND_IDS.connectHost, args: { target_key: targetKey(target) } }],
        });
      }
    }
    for (const host_id of available_hosts.current) {
      if (!existing_hosts.has(host_id)) available_hosts.current.delete(host_id);
    }
    for (const host_id of lost_hosts.current) {
      if (!existing_hosts.has(host_id)) {
        lost_hosts.current.delete(host_id);
        store?.setActions(`host-connection:${host_id}`, []);
      }
    }
  });
}
