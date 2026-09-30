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

export function vpnNeedsSignIn(status: VpnStatus | null | undefined): boolean {
  return status?.provider === "tailscale" && status.state === "starting" && Boolean(status.auth_url);
}

export function vpnRouteDetail(connection: VpnConnection, statuses: readonly VpnStatus[]): string {
  const runtime = statuses.find((status) => status.connection_id === connection.connection_id);
  if (vpnNeedsSignIn(runtime)) return "Sign in from the VPN page";
  if (runtime?.state === "connected") return "Connected";
  if (runtime?.state === "starting") return "Connecting…";
  return connection.provider === "tailscale" ? "Tailscale · Connect when needed" : "Connect when needed";
}
