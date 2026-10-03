import { describe, expect, it } from "vitest";
import type { WorkspaceHost } from "../../lib/types";
import { credentialTargets } from "./credentialTargets";

describe("credential host associations", () => {
  it("includes configured identity hints for unavailable hosts and excludes local targets", () => {
    const host: WorkspaceHost = {
      host_id: "work", name: "Work", preferred_method_id: "direct",
      connection_methods: [
        { method_id: "direct", name: "Direct", target: { kind: "ssh", destination: "work.example" } },
        { method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "work.example", vpn_connection_id: "saved-vpn" } },
        { method_id: "missing", name: "Unavailable", target: { kind: "ssh", destination: "missing.example", identity_file: "/keys/missing", unavailable: "No longer available" } },
      ],
    };
    const associations = credentialTargets([host, { host_id: "local", name: "Local", preferred_method_id: "local", connection_methods: [] }], []);
    expect(associations).toHaveLength(3);
    expect(associations.map((item) => item.name)).toEqual(["Work", "Work", "Work"]);
    expect(associations[0].target.vpn_connection_id).toBeUndefined();
    expect(associations[1].target.vpn_connection_id).toBe("saved-vpn");
    expect(associations[2].target.identity_file).toBe("/keys/missing");
    expect(associations[2].target.unavailable).toBe("No longer available");
    expect(associations.map((item) => item.target.method_id)).toEqual(["direct", "vpn", "missing"]);
  });
});


it("preserves gateway key paths for an unavailable host without inventing a usable route", () => {
  const host: WorkspaceHost = {
    host_id: "work", name: "Work", preferred_method_id: "remote",
    connection_methods: [{ method_id: "remote", name: "Remote", target: { kind: "ssh", destination: "work.example", gateway_route: [
      { gateway_id: "jump", mode: "automatic" }, { vpn_connection_id: "office" },
    ], unavailable: "Device unavailable" } }],
  };
  const [hint] = credentialTargets([host], [{ gateway_id: "jump", name: "Jump", destination: "jump.example", identity_file: "/keys/jump" }]);
  expect(hint.target.unavailable).toBe("Device unavailable");
  expect(hint.target.gateways?.[0].identity_file).toBe("/keys/jump");
  expect(hint.target.gateways?.[1]).toMatchObject({ kind: "vpn", vpn_connection_id: "office" });
});

it("uses each linked method's current complete route for credential scopes", () => {
  const hop: WorkspaceHost = { host_id: "jump", name: "Jump", preferred_method_id: "ssh",
    connection_methods: [{ method_id: "ssh", name: "SSH", target: { kind: "ssh", destination: "new.jump", gateway_route: [{ gateway_id: "edge", mode: "native_only" }] } }],
    remote_info: { remote_id: "jump-account", agent_version: "1" } };
  const host: WorkspaceHost = { host_id: "build", name: "Build", preferred_method_id: "ssh",
    connection_methods: [{ method_id: "ssh", name: "SSH", target: { kind: "ssh", destination: "build", gateway_route: [
      { host_id: "jump", method_id: "ssh", mode: "automatic" }, { vpn_connection_id: "office" },
    ] } }] };
  const targets = credentialTargets([hop, host], [{ gateway_id: "edge", name: "Edge", destination: "edge", identity_file: "/keys/edge" }]);
  expect(targets[1].target.gateways).toEqual([
    expect.objectContaining({ destination: "edge", identity_file: "/keys/edge" }),
    expect.objectContaining({ destination: "new.jump", remote_info: hop.remote_info }),
    expect.objectContaining({ kind: "vpn", vpn_connection_id: "office" }),
  ]);
  hop.connection_methods[0].target.identity_file = "/keys/unsupported-hop";
  const invalid = credentialTargets([hop, host], [{ gateway_id: "edge", name: "Edge", destination: "edge", identity_file: "/keys/edge" }])[1];
  expect(invalid.target.unavailable).toContain("private key file");
  expect(invalid.target.gateways?.map((gateway) => gateway.identity_file)).toEqual(["/keys/edge", "/keys/unsupported-hop", undefined]);
});
