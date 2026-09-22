import { describe, expect, it } from "vitest";
import type { HostCatalogDocument, RemoteIdentity, SshConnectionTarget, WorkspaceDocument } from "../../lib/types";
import { recoverRemoteHost } from "./remoteRecovery";
import {
  expectedHostIdentity,
  connectionSettings,
  hostCatalogDocument,
  hostFromTarget,
  hostTarget,
  projectedHostId,
  promoteHost,
  refreshHostCatalog,
  restoreWorkspace,
  updateHostSettings,
  workspaceDocument,
  workspaceSidebarTargets,
} from "./workspaceModel";

const empty_catalog: HostCatalogDocument = { schema_version: 1, hosts: [], ssh_gateways: [] };
const aliases = [{ destination: "build" }, { destination: "other-account" }];
const remote_info: RemoteIdentity = { remote_id: "verified-environment", agent_version: "1" };

function document(host_id?: string): WorkspaceDocument {
  return {
    schema_version: 8,
    workspace_id: "default",
    sessions: host_id ? [{ host_id, session_id: "shell", name: "Shell", last_known_cwd: null, last_known_cwd_display: null }] : [],
    tabs: host_id ? [{ host_id, session_id: "shell", kind: "session" }] : [],
    active_tab: host_id ? { host_id, session_id: "shell", kind: "session" } : null,
  };
}

describe("saved host catalog and SSH config projections", () => {
  it("projects aliases deterministically without writing their definitions", () => {
    const first = restoreWorkspace(document(), empty_catalog, aliases);
    const second = restoreWorkspace(document(), empty_catalog, [...aliases].reverse());
    expect(first.hosts.map((host) => host.host_id)).toEqual(["local", "ssh-config:build", "ssh-config:other-account"]);
    expect(second.hosts.find((host) => host.name === "build")?.host_id).toBe(first.hosts[1].host_id);
    expect(projectedHostId("name/%part")).toBe("ssh-config:name%2F%25part");
    expect(first.targets[1]).toMatchObject({ destination: "build", method_id: "ssh_config", ssh_config_alias: "build" });
    expect(hostCatalogDocument(first)).toEqual(empty_catalog);
    expect(workspaceDocument(first)).not.toHaveProperty("hosts");
    expect(workspaceDocument(first)).not.toHaveProperty("ssh_gateways");
    expect(workspaceDocument(first).host_identities).toEqual([]);
    expect(workspaceSidebarTargets(first).map((target) => target.kind)).toEqual(["local"]);
  });

  it("reveals a connected projection in the sidebar without saving its definition", () => {
    const view = restoreWorkspace(document(), empty_catalog, aliases);
    const recovered = recoverRemoteHost(view, view.targets[1] as SshConnectionTarget, remote_info)!;
    expect(workspaceSidebarTargets(recovered.view)).toEqual([recovered.view.targets[0], recovered.target]);
    const refreshed = refreshHostCatalog(recovered.view, empty_catalog, aliases);
    expect(workspaceSidebarTargets(refreshed)).toEqual(refreshed.targets.slice(0, 2));
    expect(hostCatalogDocument(refreshed)).toEqual(empty_catalog);
    expect(workspaceDocument(refreshed).host_identities).toEqual([]);
    // A connection with no remembered work remains a runtime-only choice.
    const reloaded = restoreWorkspace(workspaceDocument(refreshed), empty_catalog, aliases);
    expect(workspaceSidebarTargets(reloaded)).toEqual([reloaded.targets[0]]);
  });

  it("persists SSH-config origin on named methods and restores it at the transport boundary", () => {
    const target: SshConnectionTarget = {
      kind: "ssh", destination: "build", ssh_config_alias: "build", user: "deploy", identity_file: "~/.ssh/deploy",
      gateway_route: [{ gateway_id: "edge", mode: "native_only" }],
    };
    const host = hostFromTarget(target, "Named builder");
    const view = restoreWorkspace(document(), {
      ...empty_catalog, hosts: [host], ssh_gateways: [{ gateway_id: "edge", name: "Edge", destination: "gateway" }],
    }, aliases);
    const catalog = hostCatalogDocument(view);
    expect(catalog.hosts[0].connection_methods[0]).toMatchObject({ ssh_config_alias: "build", target: { user: "deploy" } });
    expect(catalog.hosts[0].connection_methods[0].target).not.toHaveProperty("ssh_config_alias");
    expect(connectionSettings(target)).not.toHaveProperty("ssh_config_alias");
    const reloaded = restoreWorkspace(workspaceDocument(view), catalog, aliases);
    expect(hostTarget(reloaded.hosts[1], reloaded.ssh_gateways)).toMatchObject({
      destination: "build", ssh_config_alias: "build", user: "deploy", identity_file: "~/.ssh/deploy",
      gateways: [expect.objectContaining({ destination: "gateway" })],
    });
  });

  it("infers legacy origin only for projected aliases or pure discovered aliases", () => {
    const targets: SshConnectionTarget[] = [
      { kind: "ssh", host_id: projectedHostId("build"), destination: "build", user: "deploy", identity_file: "~/.ssh/deploy" },
      { kind: "ssh", host_id: "legacy-alias", destination: "other-account" },
      { kind: "ssh", host_id: "direct", destination: "build", hostname: "10.0.0.5" },
      { kind: "ssh", host_id: "custom-account", destination: "build", user: "other" },
      { kind: "ssh", host_id: "tailnet", destination: "build", tailscale_node_id: "n123" },
      { kind: "ssh", host_id: "unrecognized", destination: "missing-alias" },
    ];
    const view = restoreWorkspace(document(), { ...empty_catalog, hosts: targets.map((target) => hostFromTarget(target)) }, aliases);
    expect(hostTarget(view.hosts[1], [])).toMatchObject({ ssh_config_alias: "build", user: "deploy" });
    expect(hostTarget(view.hosts[2], [])).toMatchObject({ ssh_config_alias: "other-account" });
    for (const host_id of ["direct", "custom-account", "tailnet", "unrecognized"]) {
      const host = view.hosts.find((host) => host.host_id === host_id)!;
      expect(host.connection_methods[0]).not.toHaveProperty("ssh_config_alias");
      expect(hostTarget(host, [])).not.toHaveProperty("ssh_config_alias");
    }
    const missing = restoreWorkspace(document(), { ...empty_catalog, hosts: [hostFromTarget(targets[0])] }, []);
    expect(hostTarget(missing.hosts[1], [])).toMatchObject({ ssh_config_alias: "build", unavailable: expect.any(String) });
  });

  it("uses a saved ID override but never merges aliases, addresses, names, or accounts", () => {
    const saved = hostFromTarget({ kind: "ssh", host_id: projectedHostId("build"), destination: "10.0.0.5" }, "Builder");
    const separate = hostFromTarget({ kind: "ssh", host_id: "separate", destination: "10.0.0.5" }, "Builder");
    const view = restoreWorkspace(document(), { ...empty_catalog, hosts: [saved, separate] }, aliases);
    expect(view.hosts).toHaveLength(4);
    expect(view.hosts[1]).toMatchObject({ host_id: projectedHostId("build"), name: "Builder", source: "saved" });
    expect(view.hosts[2]).toMatchObject({ host_id: "separate", name: "Builder" });
    expect(view.hosts[3]).toMatchObject({ source: "ssh_config", name: "other-account" });
  });

  it("hides aliases already represented by saved methods unless their projected identity is referenced", () => {
    const saved = hostFromTarget({ kind: "ssh", host_id: "existing", destination: "build" }, "Existing builder");
    const catalog = { ...empty_catalog, hosts: [saved] };
    const unreferenced = restoreWorkspace(document(), catalog, aliases);
    expect(unreferenced.hosts.map((host) => host.host_id)).toEqual(["local", "existing", projectedHostId("other-account")]);
    const referenced = restoreWorkspace(document(projectedHostId("build")), catalog, aliases);
    expect(referenced.hosts.map((host) => host.host_id)).toContain(projectedHostId("build"));
    expect(referenced.sessions[0].target).toMatchObject({ host_id: projectedHostId("build") });
    const customized = { ...saved, connection_methods: saved.connection_methods.map((method) => ({ ...method, target: { ...method.target, user: "another-user" } })) };
    expect(restoreWorkspace(document(), { ...catalog, hosts: [customized] }, aliases).hosts.map((host) => host.host_id))
      .toContain(projectedHostId("build"));
  });

  it("verifies projected hosts in memory and stores only pins for referenced hosts", () => {
    const id = projectedHostId("build");
    const view = restoreWorkspace(document(id), empty_catalog, aliases);
    const target = view.targets[1] as SshConnectionTarget;
    const recovered = recoverRemoteHost(view, target, remote_info)!;
    expect(recovered.view.hosts[1].source).toBe("ssh_config");
    expect(hostCatalogDocument(recovered.view)).toEqual(empty_catalog);
    const workspace = workspaceDocument(recovered.view);
    expect(workspace.host_identities).toEqual([{ host_id: id, remote_info }]);
    const reloaded = restoreWorkspace(workspace, empty_catalog, aliases);
    expect(reloaded.targets[1]).toMatchObject({ remote_info });
    expect(() => recoverRemoteHost(reloaded, reloaded.targets[1] as SshConnectionTarget, { ...remote_info, remote_id: "another-account" }))
      .toThrow("different remote environment");

    const unused = restoreWorkspace(document(), empty_catalog, aliases);
    const verified = recoverRemoteHost(unused, unused.targets[1] as SshConnectionTarget, remote_info)!;
    expect(workspaceDocument(verified.view).host_identities).toEqual([]);
    expect(expectedHostIdentity(refreshHostCatalog(verified.view, empty_catalog, aliases).hosts[1])).toEqual(remote_info);
  });

  it("retains unavailable alias sessions, task references, forwards, and identity pins", () => {
    const host_id = projectedHostId("build");
    const source = document(host_id);
    source.host_identities = [{ host_id, remote_info }];
    source.task_references = [{ host_id, task_id: "task", definition_id: null, applied_revision: null, is_default: false }];
    source.tabs.push({ kind: "task", host_id, task_id: "task" });
    source.port_forwards = [{ host_id, forward_id: "forward", name: "Web", enabled: true, bind_address: "127.0.0.1", local_port: 8080, remote_host: "localhost", remote_port: 80 }];
    const missing = restoreWorkspace(source, empty_catalog, []);
    expect(missing.hosts[1]).toMatchObject({ source: "unavailable", name: "build", expected_remote_info: remote_info });
    expect(missing.targets[1]).toMatchObject({ unavailable: expect.stringContaining("alias build is missing") });
    expect(() => recoverRemoteHost(missing, missing.targets[1] as SshConnectionTarget, remote_info)).toThrow("alias build is missing");
    const persisted = workspaceDocument(missing);
    expect(persisted.sessions).toEqual(source.sessions);
    expect(persisted.task_references).toEqual(source.task_references);
    expect(persisted.port_forwards).toEqual(source.port_forwards);
    expect(persisted.host_identities).toEqual(source.host_identities);
    expect(persisted.tabs).toEqual(source.tabs);
    expect(hostCatalogDocument(missing)).toEqual(empty_catalog);
    const returned = refreshHostCatalog(missing, empty_catalog, aliases);
    expect(returned.hosts[1].source).toBe("ssh_config");
    expect(returned.sessions[0].target).not.toHaveProperty("unavailable");
    expect(returned.active_tab_key).toBe(missing.active_tab_key);
  });

  it("retains deleted saved hosts as unavailable references without reconstructing definitions", () => {
    const missing = restoreWorkspace(document("saved-id"), empty_catalog, []);
    expect(missing.hosts[1]).toMatchObject({ host_id: "saved-id", source: "unavailable" });
    expect(missing.targets[1]).toMatchObject({ unavailable: expect.stringContaining("hosts.json") });
    expect(workspaceDocument(missing).sessions).toEqual(document("saved-id").sessions);
    expect(hostCatalogDocument(missing).hosts).toEqual([]);
  });

  it("promotes an explicit customization without changing session identity", () => {
    const id = projectedHostId("build");
    const view = restoreWorkspace({ ...document(id), host_identities: [{ host_id: id, remote_info }] }, empty_catalog, aliases);
    const promoted = { ...promoteHost(view.hosts[1]), name: "Build machine" };
    const edited = updateHostSettings(view, promoted);
    const catalog = hostCatalogDocument(edited);
    expect(catalog.hosts).toHaveLength(1);
    expect(catalog.hosts[0]).toMatchObject({ host_id: id, name: "Build machine", remote_info });
    expect(catalog.hosts[0]).not.toHaveProperty("source");
    const reloaded = restoreWorkspace(workspaceDocument(edited), catalog, aliases);
    expect(reloaded.hosts).toHaveLength(3);
    expect(reloaded.active_tab_key).toBe(view.active_tab_key);
    expect(reloaded.sessions[0].target).toMatchObject({ host_id: id, host_name: "Build machine", ssh_config_alias: "build" });
  });

  it("blocks a promoted alias that disappears while keeping its inline alternatives usable", () => {
    const view = restoreWorkspace(document(), empty_catalog, aliases);
    const saved = promoteHost(view.hosts[1]);
    saved.connection_methods.push({ method_id: "inline", name: "Office", target: { kind: "ssh", destination: "office", hostname: "10.0.0.5" } });
    const catalog = { ...empty_catalog, hosts: [saved] };
    const restored = restoreWorkspace(document(), catalog, []);
    expect(restored.hosts[1].source).toBe("saved");
    expect(hostTarget(restored.hosts[1], [])).toMatchObject({ unavailable: expect.stringContaining("alias build is missing") });
    expect(hostTarget(restored.hosts[1], [], "inline")).not.toHaveProperty("unavailable");
    expect(hostCatalogDocument(restored).hosts[0].connection_methods[0].target).not.toHaveProperty("unavailable");
  });

  it("keeps a workspace environment pin when the saved catalog identity changes", () => {
    const host_id = "saved";
    const saved = hostFromTarget({ kind: "ssh", host_id, destination: "build", remote_info: { ...remote_info, remote_id: "other-environment" } });
    const source = { ...document(host_id), host_identities: [{ host_id, remote_info }] };
    const restored = restoreWorkspace(source, { ...empty_catalog, hosts: [saved] }, aliases);
    expect(restored.targets[1]).toMatchObject({ remote_info });
    expect(hostCatalogDocument(restored).hosts[0].remote_info).toEqual(saved.remote_info);
    expect(() => recoverRemoteHost(restored, restored.targets[1] as SshConnectionTarget, saved.remote_info!)).toThrow("different remote environment");
  });

  it("blocks a missing promoted alias even when its method overrides authentication or port", () => {
    const view = restoreWorkspace(document(), empty_catalog, aliases);
    const saved = promoteHost(view.hosts[1]);
    saved.connection_methods[0].target = {
      ...saved.connection_methods[0].target,
      identity_file: "~/.ssh/work", user: "developer", port: 2222,
    };
    const catalog = { ...empty_catalog, hosts: [saved] };
    const missing = restoreWorkspace(document(), catalog, []);
    expect(hostTarget(missing.hosts[1], [])).toMatchObject({
      unavailable: expect.stringContaining("alias build is missing"),
    });
    const returned = refreshHostCatalog(missing, catalog, aliases);
    expect(hostTarget(returned.hosts[1], [])).not.toHaveProperty("unavailable");
  });

  it("refreshes names and source availability without replacing live transport snapshots", () => {
    const saved = hostFromTarget({ kind: "ssh", host_id: "saved", destination: "original" }, "Machine");
    const catalog = { ...empty_catalog, hosts: [saved] };
    const view = restoreWorkspace(document("saved"), catalog, aliases);
    const changed = { ...saved, name: "Renamed", connection_methods: [{ method_id: "default", name: "SSH", target: { kind: "ssh" as const, destination: "replacement" } }] };
    const refreshed = refreshHostCatalog(view, { ...catalog, hosts: [changed] }, aliases);
    expect(refreshed.targets[1]).toMatchObject({ destination: "original", host_name: "Renamed" });
    expect(refreshed.sessions[0].target).toBe(refreshed.targets[1]);
    expect(hostTarget(refreshed.hosts[1], [])).toMatchObject({ destination: "replacement" });
    const removed = refreshHostCatalog(refreshed, empty_catalog, aliases);
    expect(removed.hosts.find((host) => host.host_id === "saved")).toMatchObject({ source: "unavailable", name: "Renamed" });
    expect(removed.sessions[0].target).toMatchObject({ destination: "original", unavailable: expect.any(String) });
  });
});
