import { describe, expect, it } from "vitest";
import type {
  HostCatalogDocument,
  RemoteIdentity,
  SshConnectionTarget,
  TailscaleDevice,
  WorkspaceDocument,
} from "../../lib/types";
import { recoverRemoteHost } from "./remoteRecovery";
import {
  connectionSettings,
  hostCatalogDocument,
  hostFromTarget,
  hostTarget,
  isVirtualHost,
  promoteHost,
  refreshHostCatalog,
  restoreWorkspace,
  tailscaleHostId,
  tailscaleTarget,
  updateHostSettings,
  workspaceDocument,
  workspaceSidebarTargets,
} from "./workspaceModel";

const empty_catalog: HostCatalogDocument = { schema_version: 1, hosts: [], ssh_gateways: [] };
const device: TailscaleDevice = {
  node_id: "n123456",
  name: "Builder",
  dns_name: "builder.tail.example.ts.net",
  addresses: ["100.90.80.70", "fd7a:115c:a1e0::1"],
  online: true,
  os: "linux",
};
const remote_info: RemoteIdentity = { remote_id: "verified-builder", agent_version: "1" };

function document(host_id?: string): WorkspaceDocument {
  return {
    schema_version: 8,
    workspace_id: "default",
    sessions: host_id ? [{ host_id, session_id: "shell", name: "Shell", last_known_cwd: null, last_known_cwd_display: null }] : [],
    tabs: host_id ? [{ kind: "session", host_id, session_id: "shell" }] : [],
    active_tab: host_id ? { kind: "session", host_id, session_id: "shell" } : null,
  };
}

describe("Tailscale virtual hosts", () => {
  it("uses stable node identities and does not persist discovery definitions or sidebar choices", () => {
    const view = restoreWorkspace(document(), empty_catalog, [], [device]);
    expect(tailscaleHostId("node/%id")).toBe("tailscale:node%2F%25id");
    expect(view.hosts[1]).toMatchObject({
      host_id: tailscaleHostId(device.node_id),
      source: "tailscale",
      name: "Builder",
      tailscale_device: device,
    });
    expect(isVirtualHost(view.hosts[1])).toBe(true);
    expect(view.targets[1]).toEqual(tailscaleTarget(device));
    expect(view.targets[1]).toMatchObject({
      destination: device.dns_name,
      hostname: device.addresses[0],
      tailscale_node_id: device.node_id,
      method_id: "tailscale",
    });
    expect(hostCatalogDocument(view)).toEqual(empty_catalog);
    expect(workspaceDocument(view)).not.toHaveProperty("hosts");
    expect(workspaceDocument(view).host_identities).toEqual([]);
    expect(workspaceSidebarTargets(view)).toEqual([view.targets[0]]);
  });

  it("follows node renames and address changes without merging machines that reuse either", () => {
    const first = restoreWorkspace(document(), empty_catalog, [], [device]);
    const renamed = { ...device, name: "New name", dns_name: "new.tail.example.ts.net", addresses: ["100.90.80.71"] };
    const reused = { ...device, node_id: "n_other" };
    const next = restoreWorkspace(document(), empty_catalog, [], [renamed, reused]);
    expect(next.hosts[1].host_id).toBe(first.hosts[1].host_id);
    expect(next.hosts[1].name).toBe("New name");
    expect(next.targets[1]).toMatchObject({ destination: renamed.dns_name, hostname: renamed.addresses[0] });
    expect(next.hosts[2].host_id).not.toBe(first.hosts[1].host_id);
    const arbitrary = hostFromTarget({ kind: "ssh", host_id: "ordinary", destination: device.dns_name!, hostname: device.addresses[0] }, device.name);
    const separate = restoreWorkspace(document(), { ...empty_catalog, hosts: [arbitrary] }, [], [device]);
    expect(separate.hosts.map((host) => host.host_id)).toEqual(["local", "ordinary", tailscaleHostId(device.node_id)]);
  });

  it("uses valid IP addresses without relying on MagicDNS and rejects missing endpoints", () => {
    expect(tailscaleTarget({ ...device, dns_name: null, addresses: ["not-an-ip", "100.100.100.100"] })).toMatchObject({
      destination: "100.100.100.100", hostname: "100.100.100.100",
    });
    expect(tailscaleTarget({ ...device, dns_name: null, addresses: ["999.100.1.1", "fd7a:115c:a1e0:0000:0000:0000:0000:0001"] })).toMatchObject({
      destination: "fd7a:115c:a1e0:0000:0000:0000:0000:0001", hostname: "fd7a:115c:a1e0:0000:0000:0000:0000:0001",
    });
    expect(tailscaleTarget({ ...device, dns_name: null, addresses: [] })).toMatchObject({ unavailable: expect.stringContaining("no usable address") });
    const dns_only = tailscaleTarget({ ...device, addresses: [] });
    expect(dns_only.destination).toBe(device.dns_name);
    expect(dns_only).not.toHaveProperty("hostname");
    expect(dns_only).not.toHaveProperty("unavailable");
  });

  it("reveals a verified device without saving discovery state or its definition", () => {
    const view = restoreWorkspace(document(), empty_catalog, [], [device]);
    const recovered = recoverRemoteHost(view, view.targets[1] as SshConnectionTarget, remote_info)!;
    expect(workspaceSidebarTargets(recovered.view)).toEqual(recovered.view.targets);
    expect(recovered.view.hosts[1].source).toBe("tailscale");
    const refreshed = refreshHostCatalog(recovered.view, empty_catalog, [], [{ ...device, online: false }]);
    expect(workspaceSidebarTargets(refreshed)).toEqual(refreshed.targets);
    expect(refreshed.hosts[1].tailscale_device?.online).toBe(false);
    expect(hostTarget(refreshed.hosts[1], [])).not.toHaveProperty("unavailable");
    expect(hostCatalogDocument(refreshed)).toEqual(empty_catalog);
    expect(workspaceDocument(refreshed).host_identities).toEqual([]);
    expect(workspaceSidebarTargets(restoreWorkspace(workspaceDocument(refreshed), empty_catalog, [], [device]))).toEqual([view.targets[0]]);
  });

  it("promotes a customized projection with its existing host and method IDs", () => {
    const host_id = tailscaleHostId(device.node_id);
    const view = restoreWorkspace({ ...document(host_id), host_identities: [{ host_id, remote_info }] }, empty_catalog, [], [device]);
    const saved = { ...promoteHost(view.hosts[1]), name: "My builder" };
    saved.connection_methods[0].target = {
      ...saved.connection_methods[0].target, user: "developer", port: 2222, identity_file: "~/.ssh/work",
    };
    const edited = updateHostSettings(view, saved);
    const catalog = hostCatalogDocument(edited);
    expect(catalog.hosts[0]).toMatchObject({
      host_id, name: "My builder", remote_info, preferred_method_id: "tailscale",
      connection_methods: [{ method_id: "tailscale", tailscale_node_id: device.node_id }],
    });
    expect(catalog.hosts[0]).not.toHaveProperty("tailscale_device");
    expect(catalog.hosts[0]).not.toHaveProperty("source");
    expect(catalog.hosts[0].connection_methods[0].target).not.toHaveProperty("tailscale_node_id");
    const renamed = { ...device, name: "Discovered name changed", dns_name: "changed.ts.net", addresses: ["100.90.80.71"] };
    const restored = restoreWorkspace(workspaceDocument(edited), catalog, [], [renamed]);
    expect(restored.hosts).toHaveLength(2);
    expect(restored.hosts[1].source).toBe("saved");
    expect(restored.targets[1]).toMatchObject({
      host_id, host_name: "My builder", method_id: "tailscale", tailscale_node_id: device.node_id,
      destination: renamed.dns_name, hostname: renamed.addresses[0], user: "developer", port: 2222, identity_file: "~/.ssh/work",
    });
    expect(restored.active_tab_key).toBe(view.active_tab_key);
  });

  it("retains a provider binding when explicitly saving a discovered target", () => {
    const target = { ...tailscaleTarget(device), user: "developer" };
    const host = hostFromTarget(target, "Saved builder");
    expect(host).toMatchObject({
      host_id: target.host_id, preferred_method_id: "tailscale",
      connection_methods: [{ method_id: "tailscale", tailscale_node_id: device.node_id, target: { user: "developer" } }],
    });
    expect(hostTarget(host, [])).toMatchObject({ ...target, host_name: "Saved builder" });
    expect(connectionSettings(target)).not.toHaveProperty("tailscale_node_id");
  });

  it("retains references and identity pins while discovery is absent, then restores them", () => {
    const host_id = tailscaleHostId(device.node_id);
    const source = document(host_id);
    source.host_identities = [{ host_id, remote_info }];
    source.task_references = [{ host_id, task_id: "task", definition_id: null, applied_revision: null, is_default: false }];
    source.tabs.push({ kind: "task", host_id, task_id: "task" });
    source.port_forwards = [{ host_id, forward_id: "forward", name: "Web", enabled: true, bind_address: "127.0.0.1", local_port: 8080, remote_host: "localhost", remote_port: 80 }];
    const missing = restoreWorkspace(source, empty_catalog, [], []);
    expect(missing.hosts[1]).toMatchObject({ source: "unavailable", expected_remote_info: remote_info });
    expect(missing.targets[1]).toMatchObject({ tailscale_node_id: device.node_id, unavailable: expect.stringContaining("correct tailnet") });
    expect(() => recoverRemoteHost(missing, missing.targets[1] as SshConnectionTarget, remote_info)).toThrow("Tailscale");
    const persisted = workspaceDocument(missing);
    expect(persisted.sessions).toEqual(source.sessions);
    expect(persisted.task_references).toEqual(source.task_references);
    expect(persisted.port_forwards).toEqual(source.port_forwards);
    expect(persisted.host_identities).toEqual(source.host_identities);
    expect(persisted.tabs).toEqual(source.tabs);
    expect(hostCatalogDocument(missing)).toEqual(empty_catalog);
    const returned = refreshHostCatalog(missing, empty_catalog, [], [device]);
    expect(returned.hosts[1].source).toBe("tailscale");
    expect(returned.sessions[0].target).toMatchObject({ method_id: "tailscale", hostname: device.addresses[0], remote_info });
    expect(returned.sessions[0].target).not.toHaveProperty("unavailable");
    expect(returned.active_tab_key).toBe(missing.active_tab_key);
  });

  it("refreshes a Tailscale method on an ordinary saved host while keeping SSH and gateway settings", () => {
    const host = hostFromTarget({ kind: "ssh", host_id: "ordinary", destination: "lan", user: "developer" }, "Existing host");
    host.connection_methods.push({
      method_id: "vpn", name: "Tailnet", tailscale_node_id: device.node_id,
      target: { kind: "ssh", destination: "old.ts.net", hostname: "100.0.0.1", user: "developer", port: 2222, identity_file: "~/.ssh/id", gateway_route: [{ gateway_id: "jump", mode: "automatic" }] },
    });
    const gateway = { gateway_id: "jump", name: "Jump", destination: "jump.example" };
    const catalog = { ...empty_catalog, hosts: [host], ssh_gateways: [gateway] };
    const view = restoreWorkspace(document(), catalog, [], [device]);
    expect(view.hosts.map((item) => item.host_id)).toEqual(["local", "ordinary"]);
    expect(hostTarget(view.hosts[1], [gateway], "vpn")).toMatchObject({
      host_name: "Existing host", destination: device.dns_name, hostname: device.addresses[0], user: "developer", port: 2222,
      identity_file: "~/.ssh/id", gateway_route: [{ gateway_id: "jump", mode: "automatic" }], gateways: [{ ...gateway, mode: "automatic" }],
    });
    const missing = restoreWorkspace(document(), catalog, [], []);
    expect(hostTarget(missing.hosts[1], [gateway])).not.toHaveProperty("unavailable");
    expect(hostTarget(missing.hosts[1], [gateway], "vpn")).toMatchObject({ unavailable: expect.stringContaining("Tailscale device") });
    const persisted = hostCatalogDocument(missing);
    expect(persisted.hosts[0].connection_methods[1]).toHaveProperty("tailscale_node_id", device.node_id);
    expect(persisted.hosts[0].connection_methods[1].target).not.toHaveProperty("unavailable");
    expect(persisted.hosts[0]).not.toHaveProperty("tailscale_device");
    const returned = refreshHostCatalog(missing, persisted, [], [device]);
    expect(hostTarget(returned.hosts[1], [gateway], "vpn")).not.toHaveProperty("unavailable");
  });

  it("retains separately referenced virtual identities and saved accounts rather than merging them", () => {
    const first = hostFromTarget({ ...tailscaleTarget(device), host_id: "account-one", user: "one" });
    const second = hostFromTarget({ ...tailscaleTarget(device), host_id: "account-two", user: "two" });
    const catalog = { ...empty_catalog, hosts: [first, second] };
    expect(restoreWorkspace(document(), catalog, [], [device]).hosts.map((host) => host.host_id)).toEqual(["local", "account-one", "account-two"]);
    const referenced = restoreWorkspace(document(tailscaleHostId(device.node_id)), catalog, [], [device]);
    expect(referenced.hosts.map((host) => host.host_id)).toEqual(["local", "account-one", "account-two", tailscaleHostId(device.node_id)]);
    expect(referenced.sessions[0].target).toMatchObject({ host_id: tailscaleHostId(device.node_id) });
  });

  it("refreshes discovered settings without changing live transport snapshots", () => {
    const host_id = tailscaleHostId(device.node_id);
    const view = restoreWorkspace(document(host_id), empty_catalog, [], [device]);
    const changed = { ...device, name: "Renamed", dns_name: "replacement.ts.net", addresses: ["100.90.80.71"] };
    const refreshed = refreshHostCatalog(view, empty_catalog, [], [changed]);
    expect(refreshed.targets[1]).toMatchObject({ destination: device.dns_name, hostname: device.addresses[0], host_name: "Renamed" });
    expect(refreshed.sessions[0].target).toBe(refreshed.targets[1]);
    expect(hostTarget(refreshed.hosts[1], [])).toMatchObject({ destination: changed.dns_name, hostname: changed.addresses[0] });
    const edited = updateHostSettings(refreshed, { ...promoteHost(refreshed.hosts[1]), name: "My host" });
    expect(edited.sessions[0].target).toMatchObject({ destination: device.dns_name, hostname: device.addresses[0], host_name: "My host" });
    const missing = refreshHostCatalog(refreshed, empty_catalog, [], []);
    expect(missing.hosts[1]).toMatchObject({ source: "unavailable", name: "Renamed" });
    expect(missing.sessions[0].target).toMatchObject({ destination: device.dns_name, unavailable: expect.any(String) });
  });
});
