import { describe, expect, it } from "vitest";
import type { ResolvedSshGateway, SshConnectionTarget } from "../../lib/types";
import { remoteVpnUpdateOwner } from "./remoteVpnRecovery";
import { resolveVpnRouteStep } from "./sshRoute";

const first = { kind: "ssh" as const, gateway_id: "edge", name: "Office edge", destination: "edge",
  user: "operator", port: 2222, mode: "native_only" as const,
  remote_info: { remote_id: "edge-account", agent_version: "0.1.0" } };
const second = { kind: "ssh" as const, gateway_id: "host:jump:office", name: "Jump host", destination: "jump",
  hostname: "10.0.0.7", user: "alice", mode: "automatic" as const,
  remote_info: { remote_id: "jump-account", agent_version: "0.1.0" } };
const remote_vpn = resolveVpnRouteStep({ vpn_connection_id: "company" });
const target: SshConnectionTarget = { kind: "ssh", host_id: "build", destination: "build", remote_info: { remote_id: "build-account", agent_version: "0.1.0" },
  gateway_route: [{ host_id: "removed-saved-host", method_id: "removed-method", mode: "automatic" }],
  gateways: [first, remote_vpn, second, remote_vpn] };
const failure = (vpn_route_index: unknown) => ({ code: "remote_vpn_components_update_required", message: "Update the VPN host.", vpn_route_index });

describe("remote VPN component update recovery", () => {
  it("selects a later VPN's precise owner from the failed runtime snapshot", () => {
    const before = JSON.stringify(target);
    expect(remoteVpnUpdateOwner(target, failure(1))).toEqual({ name: "Jump host", target: {
      kind: "ssh", destination: "jump", hostname: "10.0.0.7", ssh_config_alias: "jump", user: "alice",
      remote_info: second.remote_info, use_ssh_config_master: false, gateways: [first, remote_vpn],
    } });
    expect(JSON.stringify(target)).toBe(before);
    expect(remoteVpnUpdateOwner(target, failure(0))).toEqual({ name: "Office edge", target: {
      kind: "ssh", destination: "edge", user: "operator", port: 2222, remote_info: first.remote_info, use_ssh_config_master: false,
    } });
  });

  it("counts the legacy local VPN before remote VPNs and includes it in the owner prefix", () => {
    const legacy = { ...target, vpn_connection_id: "local-vpn" };
    expect(remoteVpnUpdateOwner(legacy, failure(0))).toBeNull();
    expect(remoteVpnUpdateOwner(legacy, failure(2))).toEqual({ name: "Jump host", target: {
      kind: "ssh", destination: "jump", hostname: "10.0.0.7", ssh_config_alias: "jump", user: "alice",
      remote_info: second.remote_info, use_ssh_config_master: false,
      gateways: [resolveVpnRouteStep({ vpn_connection_id: "local-vpn" }), first, remote_vpn],
    } });
  });

  it.each([undefined, null, "1", -1, 0.5, Number.NaN, Number.POSITIVE_INFINITY, 99])(
    "does not infer an update owner from malformed or unavailable ordinal %s", (index) => {
      expect(remoteVpnUpdateOwner(target, failure(index))).toBeNull();
    },
  );

  it("requires the targeted error and a resolved SSH execution host", () => {
    expect(remoteVpnUpdateOwner(target, { ...failure(1), code: "protocol_version_mismatch" })).toBeNull();
    expect(remoteVpnUpdateOwner(target, "remote_vpn_components_update_required")).toBeNull();
    expect(remoteVpnUpdateOwner({ kind: "local" }, failure(1))).toBeNull();
    expect(remoteVpnUpdateOwner({ ...target, gateways: undefined }, failure(0))).toBeNull();
    for (const gateways of [[remote_vpn], [remote_vpn, remote_vpn], [
      { ...first, kind: "socks5" }, remote_vpn,
    ]] as ResolvedSshGateway[][]) {
      expect(remoteVpnUpdateOwner({ ...target, gateways }, failure(gateways.filter((gateway) => gateway.kind === "vpn").length - 1))).toBeNull();
    }
  });
});
