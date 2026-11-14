import { translate, type Locale } from "../i18n/messages";
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

const STATE_MESSAGES: Record<VpnState, string> = {
  starting: "vpn.connecting",
  connected: "vpn.connected",
  stopping: "vpn.disconnecting",
  stopped: "vpn.disconnected",
};

export function vpnStateLabel(state: VpnState, locale: Locale): string {
  return translate(locale, STATE_MESSAGES[state]);
}

export function vpnRouteDetail(connection: VpnConnection, statuses: readonly VpnStatus[], locale: Locale = "en"): string {
  const runtime = statuses.find((status) => status.connection_id === connection.connection_id);
  if (runtime?.status_unavailable) return translate(locale, "vpn.status_unavailable");
  if (vpnNeedsSignIn(runtime)) return translate(locale, "vpn.sign_in_page");
  if (runtime?.state === "connected") return translate(locale, "vpn.connected");
  if (runtime?.state === "starting") return translate(locale, "vpn.connecting");
  return translate(locale, connection.provider === "tailscale" ? "vpn.tailscale_connect_when_needed" : "vpn.connect_when_needed");
}
