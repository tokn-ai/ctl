import { describe, expect, it } from "vitest";
import type { HostCatalogDocument, SshConnectionTarget, WorkspaceDocument, WorkspaceHost } from "../../lib/types";
import { expandSshRoute, localVpnConnectionId, resolvedVpnExecutionTarget, vpnExecutionTarget } from "./sshRoute";
import { connectionSettings, hostCatalogDocument, hostTarget, projectedHostId, refreshHostCatalog, resolveSshGateways, restoreWorkspace, tailscaleHostId, usesSshConfigMaster, workspaceDocument, workspaceSidebarTargets } from "./workspaceModel";
import { hostConnectionTargets } from "./useHostConnections";

function host(host_id: string, target: SshConnectionTarget, method_id = "ssh"): WorkspaceHost {
  return { host_id, name: host_id, preferred_method_id: method_id,
    connection_methods: [{ method_id, name: method_id, target }] };
}
const document: WorkspaceDocument = { schema_version: 8, workspace_id: "default", sessions: [], tabs: [], active_tab: null };
const edge = { gateway_id: "edge", name: "Edge", destination: "edge.example" };
const pin = { remote_id: "selected-hop-account", agent_version: "1" };

describe("linked saved host routes", () => {
  it("expands selected methods recursively in order, preserving links and the current account pin", () => {
    const jump = { ...host("jump", { kind: "ssh", destination: "alice@old-address", hostname: "10.1.2.3",
      vpn_connection_id: "local", gateway_route: [{ gateway_id: "edge", mode: "native_only" }] }),
      remote_info: { remote_id: "old-pin", agent_version: "1" }, expected_remote_info: pin };
    jump.connection_methods[0].ssh_config_alias = "alice@jump-alias";
    const inner = host("inner", { kind: "ssh", destination: "inner.example", gateway_route: [
      { host_id: "jump", method_id: "ssh", mode: "automatic" }, { vpn_connection_id: "office" },
    ] });
    const target: SshConnectionTarget = { kind: "ssh", host_id: "target", destination: "build", gateway_route: [
      { host_id: "inner", method_id: "ssh", mode: "agent_relay_only" }, { vpn_connection_id: "office" },
    ] };
    const resolved = resolveSshGateways(target, [edge], [jump, inner]);
    expect(resolved.gateways?.map((gateway) => gateway.destination)).toEqual(["local", "edge.example", "jump-alias", "office", "inner.example", "office"]);
    expect(resolved.gateways?.[2]).toMatchObject({ gateway_id: "host:jump:ssh", name: "jump", user: "alice", hostname: "10.1.2.3", remote_info: pin });
    expect(resolved.gateways?.[4].mode).toBe("agent_relay_only");
    expect(connectionSettings(resolved)).toEqual(target.kind === "ssh" ? { kind: "ssh", destination: "build", gateway_route: target.gateway_route } : target);
    expect(localVpnConnectionId(resolved)).toBe("local");
    expect(resolvedVpnExecutionTarget(resolved.gateways!, 3)).toMatchObject({ destination: "jump-alias", ssh_config_alias: "jump-alias", user: "alice", remote_info: pin,
      use_ssh_config_master: false, gateways: [{ kind: "vpn", vpn_connection_id: "local" }, { destination: "edge.example" }] });
    expect(vpnExecutionTarget(target.gateway_route!, 1, [edge], [jump, inner], target.host_id)).toMatchObject({ destination: "inner.example", gateways: resolved.gateways!.slice(0, 4) });
  });

  it("uses current linked settings for new scopes and retains old runtime snapshots", () => {
    const jump = host("jump", { kind: "ssh", destination: "old.jump" });
    const target = host("target", { kind: "ssh", destination: "build", gateway_route: [{ host_id: "jump", method_id: "ssh", mode: "automatic" }] });
    const snapshot = hostTarget(target, [], undefined, [jump, target]) as SshConnectionTarget;
    const updated = { ...jump, connection_methods: [{ ...jump.connection_methods[0], target: { kind: "ssh" as const, destination: "new.jump" } }] };
    const methods = hostConnectionTargets(target, { gateways: [], hosts: [updated, target], targets: [snapshot] });
    expect(methods.map((method) => method.target.gateways?.[0].destination)).toEqual(["new.jump", "old.jump"]);
    expect(snapshot.gateways?.[0].destination).toBe("old.jump");
    expect(snapshot.gateway_route).toEqual(target.connection_methods[0].target.gateway_route);
    // A removed link blocks new scopes but a retained master can still be stopped.
    expect(hostConnectionTargets(target, { gateways: [], hosts: [target], targets: [snapshot] }))
      .toEqual([{ name: "Previous connection", target: snapshot }]);
  });

  it("retains repeated host links and gives explicit user settings precedence over address user", () => {
    const jump = host("jump", { kind: "ssh", destination: "alice@jump", user: "operator" });
    const route = [{ host_id: "jump", method_id: "ssh", mode: "automatic" as const }, { host_id: "jump", method_id: "ssh", mode: "native_only" as const }];
    expect(expandSshRoute({ kind: "ssh", destination: "build", gateway_route: route }, [], [jump])).toEqual([
      expect.objectContaining({ destination: "jump", user: "operator", mode: "automatic" }),
      expect.objectContaining({ destination: "jump", user: "operator", mode: "native_only" }),
    ]);
  });

  it("uses a private master when a linked SSH alias has a hostname override", () => {
    const jump = host("jump", { kind: "ssh", destination: "jump", hostname: "10.1.2.3" });
    jump.connection_methods[0].ssh_config_alias = "jump-alias";
    const target: SshConnectionTarget = { kind: "ssh", destination: "build", ssh_config_alias: "build", use_ssh_config_master: true,
      gateway_route: [{ host_id: "jump", method_id: "ssh", mode: "automatic" }] };
    expect(usesSshConfigMaster(resolveSshGateways(target, [], [jump]))).toBe(false);
    delete jump.connection_methods[0].target.hostname;
    expect(usesSshConfigMaster(resolveSshGateways(target, [], [jump]))).toBe(true);
  });

  it("fails missing links, methods, unavailable methods, and hop key settings without losing the saved references", () => {
    const target: SshConnectionTarget = { kind: "ssh", host_id: "target", destination: "build", gateway_route: [{ host_id: "jump", method_id: "ssh", mode: "automatic" }] };
    const variants: Array<[WorkspaceHost[], RegExp]> = [
      [[], /SSH host.*missing/],
      [[host("jump", { kind: "ssh", destination: "jump" }, "deleted")], /connection method.*missing/],
      [[host("jump", { kind: "ssh", destination: "jump", unavailable: "Alias missing" })], /Alias missing/],
      [[host("jump", { kind: "ssh", destination: "jump", identity_file: "\/keys\/jump" })], /private key file/],
    ];
    for (const [hosts, error] of variants) {
      expect(() => resolveSshGateways(target, [], hosts)).toThrow(error);
      expect(hostTarget(host("target", target), [], undefined, hosts)).toMatchObject({ unavailable: expect.stringMatching(error), gateway_route: target.gateway_route });
    }
  });

  it("detects destination and indirect cycles and checks the flattened limit including VPNs", () => {
    const target: SshConnectionTarget = { kind: "ssh", host_id: "target", destination: "build", gateway_route: [{ host_id: "jump", method_id: "ssh", mode: "automatic" }] };
    const jump = host("jump", { kind: "ssh", destination: "jump", gateway_route: [{ host_id: "target", method_id: "ssh", mode: "automatic" }] });
    expect(() => resolveSshGateways(target, [], [jump, host("target", { kind: "ssh", destination: "build" })])).toThrow(/cycle/);
    const other = host("other", { kind: "ssh", destination: "other", gateway_route: [{ host_id: "jump", method_id: "ssh", mode: "automatic" }] });
    jump.connection_methods[0].target.gateway_route = [{ host_id: "other", method_id: "ssh", mode: "automatic" }];
    expect(() => resolveSshGateways(target, [], [jump, other])).toThrow(/cycle/);
    jump.connection_methods[0].target = { kind: "ssh", destination: "jump", vpn_connection_id: "office",
      gateway_route: Array.from({ length: 7 }, () => ({ gateway_id: "edge", mode: "automatic" })) };
    expect(() => resolveSshGateways(target, [edge], [jump])).toThrow(/more than 8 hops/);
    jump.connection_methods[0].target.gateway_route!.pop();
    expect(resolveSshGateways(target, [edge], [jump]).gateways).toHaveLength(8);
  });

  it("validates VPN adjacency across inherited route boundaries", () => {
    const jump = host("jump", { kind: "ssh", destination: "jump", vpn_connection_id: "office" });
    const target: SshConnectionTarget = { kind: "ssh", destination: "build", vpn_connection_id: "local",
      gateway_route: [{ host_id: "jump", method_id: "ssh", mode: "automatic" }] };
    expect(() => resolveSshGateways(target, [], [jump])).toThrow(/VPN after VPN or SOCKS/);
    expect(() => resolveSshGateways({ ...target, vpn_connection_id: undefined, gateway_route: [
      { gateway_id: "socks", mode: "automatic" }, ...target.gateway_route!,
    ] }, [{ ...edge, gateway_id: "socks", kind: "socks5" }], [jump])).toThrow(/VPN after VPN or SOCKS/);
  });

  it("bounds recursive descent before expanding a deeply linked catalog", () => {
    const hosts = Array.from({ length: 1024 }, (_, index) => host(`jump-${index}`, { kind: "ssh", destination: `jump-${index}`,
      ...(index < 1023 ? { gateway_route: [{ host_id: `jump-${index + 1}`, method_id: "ssh", mode: "automatic" as const }] } : {}) }));
    const target: SshConnectionTarget = { kind: "ssh", destination: "build", gateway_route: [{ host_id: "jump-0", method_id: "ssh", mode: "automatic" }] };
    expect(() => resolveSshGateways(target, [], hosts)).toThrow(/more than 8 hops/);
    hosts[7].connection_methods[0].target.gateway_route = [];
    expect(resolveSshGateways(target, [], hosts).gateways).toHaveLength(8);
  });

  it("resolves projected hop methods and remembers pins when the hop is only referenced by a route", () => {
    const hop_id = projectedHostId("jump");
    const target = host("target", { kind: "ssh", destination: "build", gateway_route: [{ host_id: hop_id, method_id: "ssh_config", mode: "automatic" }] });
    const catalog: HostCatalogDocument = { schema_version: 1, hosts: [target], ssh_gateways: [] };
    const saved = { ...document, host_identities: [{ host_id: hop_id, remote_info: pin }] };
    const view = restoreWorkspace(saved, catalog, [{ destination: "jump" }]);
    expect(view.targets[1]).toMatchObject({ gateways: [{ destination: "jump", remote_info: pin }] });
    expect(workspaceSidebarTargets(view).map((candidate) => candidate.kind === "ssh" ? candidate.host_id : "local")).toContain(hop_id);
    expect(workspaceDocument(view).host_identities).toEqual(saved.host_identities);
    expect(hostCatalogDocument(view).hosts).toHaveLength(1);
    const missing = restoreWorkspace(saved, catalog, []);
    expect(missing.targets[1]).toMatchObject({ unavailable: expect.stringContaining("jump"), gateway_route: target.connection_methods[0].target.gateway_route });
  });

  it("refreshes Tailscale hop addresses for new connections while existing sessions keep their route", () => {
    const device = { node_id: "node", name: "Jump", dns_name: "old.tail.net", addresses: ["100.1.2.3"], online: true, os: "linux" };
    const target = host("target", { kind: "ssh", destination: "build", gateway_route: [{ host_id: tailscaleHostId("node"), method_id: "tailscale", mode: "automatic" }] });
    const catalog: HostCatalogDocument = { schema_version: 1, hosts: [target], ssh_gateways: [] };
    const saved: WorkspaceDocument = { ...document, sessions: [{ host_id: "target", session_id: "shell", name: "Shell", last_known_cwd: null, last_known_cwd_display: null }] };
    const before = restoreWorkspace(saved, catalog, [], [device]);
    const changed = refreshHostCatalog(before, catalog, [], [{ ...device, dns_name: "new.tail.net", addresses: ["100.4.5.6"] }]);
    expect((hostTarget(changed.hosts[1], [], undefined, changed.hosts) as SshConnectionTarget).gateways?.[0]).toMatchObject({ destination: "new.tail.net", hostname: "100.4.5.6" });
    expect((changed.sessions[0].target as SshConnectionTarget).gateways?.[0]).toMatchObject({ destination: "old.tail.net", hostname: "100.1.2.3" });
  });
});
