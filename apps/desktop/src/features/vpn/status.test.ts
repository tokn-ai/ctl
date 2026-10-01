import { describe, expect, it } from "vitest";
import type { VpnStatus } from "../../lib/types";
import { vpnActivity } from "./status";

function runtime(vpn_id: string, overrides: Partial<VpnStatus> = {}): VpnStatus {
  return {
    vpn_id, state: "connected", running: true, connection_id: null,
    endpoint: "socks5h://127.0.0.1:49152", container_name: "example-container", ...overrides,
  };
}

describe("VPN activity observations", () => {
  it("distinguishes confirmed empty inventory from unavailable empty inventory", () => {
    expect(vpnActivity([], new Set(), [])).toEqual({ state: "stopped", active_count: 0, inventory_incomplete: false });
    expect(vpnActivity([], new Set(), ["Container engine unavailable"])).toEqual({ state: "stopped", active_count: 0, inventory_incomplete: true });
  });

  it("counts confirmed shared VPNs while excluding retained uncertain states", () => {
    const statuses = [
      runtime("local", { shared_container: true, locally_connected: true }),
      runtime("shared", { shared_container: true, locally_connected: false }),
      runtime("unavailable", { status_unavailable: true }),
      runtime("cached"),
    ];
    expect(vpnActivity(statuses, new Set(["cached"]), [])).toEqual({ state: "connected", active_count: 2, inventory_incomplete: true });
    expect(vpnActivity(statuses.slice(2), new Set(["cached"]), [])).toEqual({ state: "stopped", active_count: 0, inventory_incomplete: true });
  });
});
