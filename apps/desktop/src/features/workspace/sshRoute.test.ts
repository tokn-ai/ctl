import { describe, expect, it } from "vitest";
import type { SshConnectionTarget, SshGatewayRouteStep } from "../../lib/types";
import { hasVpnRoute, localVpnConnectionId, orderedSshRoute, vpnExecutionTarget } from "./sshRoute";
import { connectionSettings, resolveSshGateways, usesSshConfigMaster } from "./workspaceModel";

const first = { gateway_id: "first", name: "First", destination: "first.example",
  remote_info: { remote_id: "first-account", agent_version: "1" } };
const second = { gateway_id: "second", name: "Second", destination: "second.example", user: "operator" };
const route: SshGatewayRouteStep[] = [
  { vpn_connection_id: "local" }, { gateway_id: first.gateway_id, mode: "native_only" },
  { vpn_connection_id: "office" }, { gateway_id: second.gateway_id, mode: "automatic" },
  { vpn_connection_id: "office" },
];

describe("ordered SSH and VPN routes", () => {
  it("resolves mixed routes in order and persists stable references only", () => {
    const target: SshConnectionTarget = { kind: "ssh", destination: "build", gateway_route: route };
    const resolved = resolveSshGateways(target, [first, second]);
    expect(resolved.gateways?.map((gateway) => gateway.kind ?? "ssh")).toEqual(["vpn", "ssh", "vpn", "ssh", "vpn"]);
    expect(resolved.gateways?.[0]).toEqual({ kind: "vpn", gateway_id: "vpn:local", name: "local", destination: "local",
      vpn_connection_id: "local", mode: "automatic" });
    expect(connectionSettings(resolved)).toEqual(target);
    expect(hasVpnRoute(resolved)).toBe(true);
    expect(usesSshConfigMaster({ ...resolved, ssh_config_alias: "build", use_ssh_config_master: true })).toBe(false);
  });

  it("uses only first-position VPNs for local status and sign-in", () => {
    expect(localVpnConnectionId({ kind: "ssh", destination: "build", gateway_route: route })).toBe("local");
    expect(localVpnConnectionId({ kind: "ssh", destination: "build", gateway_route: route.slice(1) })).toBeUndefined();
    expect(localVpnConnectionId({ kind: "ssh", destination: "build", vpn_connection_id: "legacy", gateway_route: route.slice(1) })).toBe("legacy");
  });

  it("constructs each execution host through only the prefix before that SSH hop", () => {
    expect(vpnExecutionTarget(route, 0, [first, second])).toBeUndefined();
    expect(vpnExecutionTarget(route, 2, [first, second])).toMatchObject({
      kind: "ssh", destination: first.destination, remote_info: first.remote_info,
      gateway_route: route.slice(0, 1), gateways: [{ kind: "vpn", vpn_connection_id: "local" }],
    });
    const owner = vpnExecutionTarget(route, 4, [first, second]);
    expect(owner).toMatchObject({ kind: "ssh", destination: second.destination, user: second.user, gateway_route: route.slice(0, 3) });
    expect(owner?.gateways?.map((gateway) => gateway.kind ?? "ssh")).toEqual(["vpn", "ssh", "vpn"]);
  });

  it("puts the legacy local VPN before an ordered route", () => {
    expect(orderedSshRoute({ kind: "ssh", destination: "build", vpn_connection_id: "legacy", gateway_route: route.slice(1) }))
      .toEqual([{ vpn_connection_id: "legacy" }, ...route.slice(1)]);
  });
});
