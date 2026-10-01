import type { VpnConnection, VpnState, VpnStatus } from "../../lib/types";

export function vpnRuntimeId(status: VpnStatus): string {
  return status.vpn_id ?? status.connection_id ?? status.container_name ?? "legacy";
}

export function vpnAggregateState(statuses: readonly VpnStatus[]): VpnState {
  if (statuses.some((status) => status.state === "connected")) return "connected";
  if (statuses.some((status) => status.state === "starting")) return "starting";
  if (statuses.some((status) => status.state === "stopping")) return "stopping";
  return "stopped";
}

/** Activity describes observed containers; incomplete inventory cannot prove disconnection. */
export function vpnActivity(statuses: readonly VpnStatus[], uncertain_ids: ReadonlySet<string>, discovery_warnings: readonly string[]) {
  const active = statuses.filter((status) => status.state !== "stopped");
  const observed = active.filter((status) => !status.status_unavailable && !uncertain_ids.has(vpnRuntimeId(status)));
  return {
    state: vpnAggregateState(observed),
    active_count: observed.length,
    inventory_incomplete: discovery_warnings.length > 0 || uncertain_ids.size > 0 || observed.length !== active.length,
  };
}

export function vpnNeedsSignIn(status: VpnStatus | null | undefined): boolean {
  return status?.provider === "tailscale" && !status.status_unavailable && status.state === "starting" && Boolean(status.auth_url);
}

export function vpnRouteDetail(connection: VpnConnection, statuses: readonly VpnStatus[]): string {
  const runtime = statuses.find((status) => status.connection_id === connection.connection_id);
  if (runtime?.status_unavailable) return "Status unavailable";
  if (vpnNeedsSignIn(runtime)) return "Sign in from the VPN page";
  if (runtime?.state === "connected") return "Connected";
  if (runtime?.state === "starting") return "Connecting…";
  return connection.provider === "tailscale" ? "Tailscale · Connect when needed" : "Connect when needed";
}
