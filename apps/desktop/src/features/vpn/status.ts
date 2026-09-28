import type { VpnState, VpnStatus } from "../../lib/types";

export function vpnRuntimeId(status: VpnStatus): string {
  return status.vpn_id ?? status.connection_id ?? status.container_name ?? "legacy";
}

export function vpnAggregateState(statuses: readonly VpnStatus[]): VpnState {
  if (statuses.some((status) => status.state === "connected")) return "connected";
  if (statuses.some((status) => status.state === "starting")) return "starting";
  if (statuses.some((status) => status.state === "stopping")) return "stopping";
  return "stopped";
}
