import { describe, expect, it } from "vitest";
import type { WorkspaceHost } from "../../lib/types";
import { credentialTargets } from "./credentialTargets";

describe("credential host associations", () => {
  it("includes every configured route and excludes unavailable or local targets", () => {
    const host: WorkspaceHost = {
      host_id: "work", name: "Work", preferred_method_id: "direct",
      connection_methods: [
        { method_id: "direct", name: "Direct", target: { kind: "ssh", destination: "work.example" } },
        { method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "work.example", vpn_connection_id: "saved-vpn" } },
        { method_id: "missing", name: "Unavailable", target: { kind: "ssh", destination: "missing.example", unavailable: "No longer available" } },
      ],
    };
    const associations = credentialTargets([host, { host_id: "local", name: "Local", preferred_method_id: "local", connection_methods: [] }], []);
    expect(associations).toHaveLength(2);
    expect(associations.map((item) => item.name)).toEqual(["Work", "Work"]);
    expect(associations[0].target.vpn_connection_id).toBeUndefined();
    expect(associations[1].target.vpn_connection_id).toBe("saved-vpn");
    expect(associations.map((item) => item.target.method_id)).toEqual(["direct", "vpn"]);
  });
});
