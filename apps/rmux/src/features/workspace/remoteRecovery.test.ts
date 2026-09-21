import { describe, expect, it } from "vitest";
import { recoverRemoteHost, sameSshEndpoint } from "./remoteRecovery";
import { hostCatalogDocument, hostTarget, restoreWorkspace, tailscaleHostId, workspaceDocument } from "./workspaceModel";
import type { RemoteIdentity, SshConnectionTarget, LegacyWorkspaceDocument, TailscaleDevice } from "../../lib/types";

const remote_info: RemoteIdentity = {
  remote_id: "b765c444-28d0-4772-bb16-d3b212290bcb",
  agent_version: "0.1.0",
};

function document(): LegacyWorkspaceDocument {
  return {
    schema_version: 3,
    workspace_id: "default",
    hosts: [
      { host_id: "local", target: { kind: "local" } },
      { host_id: "old", target: { kind: "ssh", destination: "old-ip", user: "alice", remote_info } },
    ],
    sessions: [{ host_id: "old", session_id: "shell", name: "work", last_known_cwd: "/work", last_known_cwd_display: "~/work" }],
    tabs: [{ host_id: "old", session_id: "shell" }],
    active_tab: { host_id: "old", session_id: "shell" },
    task_references: [{ host_id: "old", task_id: "job", definition_id: null, applied_revision: null, is_default: false }],
    port_forwards: [{ host_id: "old", forward_id: "http", name: "Preview", enabled: true, bind_address: "127.0.0.1", local_port: 8080, remote_host: "127.0.0.1", remote_port: 80 }],
  };
}

describe("remote environment recovery", () => {
  it("switches an explicitly selected method while preserving host ownership and the preferred method", () => {
    const before = restoreWorkspace(document());
    before.hosts[1].connection_methods.push({
      method_id: "vpn",
      name: "VPN",
      target: { kind: "ssh", destination: "new-name", hostname: "10.0.0.5", user: "alice" },
    });
    const candidate = hostTarget(before.hosts[1], [], "vpn");
    if (candidate.kind !== "ssh") throw new Error("Expected SSH target");
    const identity = { ...remote_info, agent_version: "0.2.0" };
    const recovered = recoverRemoteHost(before, candidate, identity)!;

    expect(recovered.target).toMatchObject({
      host_id: "old", host_name: "old-ip", method_id: "vpn",
      hostname: "10.0.0.5", remote_info: identity,
    });
    expect(recovered.view.active_tab_key).toBe(before.active_tab_key);
    expect(recovered.view.tab_order).toEqual(before.tab_order);
    expect(recovered.view.shell_states).toEqual(before.shell_states);
    expect(recovered.view.task_references).toEqual(before.task_references);
    expect(recovered.view.port_forwards).toEqual(before.port_forwards);
    expect(recovered.view.tabs[0].target).toBe(recovered.target);
    expect(recovered.view.sessions[0].target).toBe(recovered.target);
    expect(recovered.key_changes.size).toBe(0);
    expect(recovered.view.hosts[1]).toEqual({ ...before.hosts[1], remote_info: identity });

    const reloaded = restoreWorkspace(workspaceDocument(recovered.view), hostCatalogDocument(recovered.view));
    expect(reloaded.targets[1]).toMatchObject({ destination: "old-ip", method_id: "default", remote_info: identity });
    expect(reloaded.hosts[1].connection_methods).toHaveLength(2);
    expect(before.targets[1]).toMatchObject({ destination: "old-ip", remote_info });
  });

  it("never merges separate saved hosts that share an address or verified remote identity", () => {
    const saved = document();
    saved.hosts.push({ host_id: "alias", target: { kind: "ssh", destination: "old-ip", user: "alice", remote_info } });
    saved.sessions.push(...["shell", "other"].map((session_id) => ({ ...saved.sessions[0], host_id: "alias", session_id })));
    saved.tabs.push({ host_id: "alias", session_id: "shell" }, { host_id: "alias", session_id: "other" });
    saved.active_tab = saved.tabs[2];
    saved.task_references!.push({ host_id: "alias", task_id: "job", definition_id: null, applied_revision: null, is_default: false });
    saved.tabs.push({ kind: "task", host_id: "alias", task_id: "job" });
    const before = restoreWorkspace(saved);
    const identity = { ...remote_info, agent_version: "0.2.0" };
    const recovered = recoverRemoteHost(before, { kind: "ssh", destination: "old-ip", host_id: "alias", method_id: "default" }, identity)!;
    const persisted = workspaceDocument(recovered.view);

    expect(recovered.target.host_id).toBe("alias");
    const catalog = hostCatalogDocument(recovered.view);
    expect(catalog.hosts).toHaveLength(2);
    expect(catalog.hosts[0].remote_info).toEqual(remote_info);
    expect(catalog.hosts[1].remote_info).toEqual(identity);
    expect(persisted.sessions).toEqual(saved.sessions);
    expect(persisted.tabs).toEqual([
      { kind: "session", host_id: "old", session_id: "shell" },
      { kind: "session", host_id: "alias", session_id: "shell" },
      { kind: "session", host_id: "alias", session_id: "other" },
      { kind: "task", host_id: "alias", task_id: "job" },
    ]);
    expect(persisted.active_tab).toEqual({ kind: "session", host_id: "alias", session_id: "other" });
    expect(persisted.task_references).toEqual(saved.task_references);
    expect(persisted.port_forwards).toEqual(saved.port_forwards);
    expect(recovered.view.sessions[0]).toBe(before.sessions[0]);
    expect(recovered.view.tabs[0]).toBe(before.tabs[0]);
  });

  it("requires an explicit saved host instead of inferring one from an address or remote identity", () => {
    const before = restoreWorkspace(document());
    expect(recoverRemoteHost(before, { kind: "ssh", destination: "old-ip" }, remote_info)).toBeNull();
    expect(recoverRemoteHost(before, { kind: "ssh", destination: "new-ip" }, remote_info)).toBeNull();
    expect(() => recoverRemoteHost(before, { kind: "ssh", destination: "old-ip", host_id: "unknown" }, remote_info))
      .toThrow("host is no longer in the workspace");
  });

  it("rejects another remote environment without changing the saved identity or references", () => {
    const before = restoreWorkspace(document());
    const saved = workspaceDocument(before);
    const other = { ...remote_info, remote_id: "2c07a8fa-0c24-452c-bbbb-8a57fe89a035" };

    expect(() => recoverRemoteHost(before, {
      kind: "ssh", destination: "old-ip", host_id: "old", method_id: "default",
    }, other)).toThrow("different remote environment");
    expect(workspaceDocument(before)).toEqual(saved);
  });

  it("learns a legacy host identity using its preferred method without changing references", () => {
    const saved = document();
    saved.hosts[1] = { host_id: "old", target: { kind: "ssh", destination: "old-ip" } };
    const before = restoreWorkspace(saved);
    const result = recoverRemoteHost(before, { kind: "ssh", destination: "old-ip", host_id: "old" }, remote_info)!;

    expect(result.view.active_tab_key).toBe(before.active_tab_key);
    expect(result.target).toMatchObject({ remote_info, method_id: "default" });
    expect(hostCatalogDocument(result.view).hosts[0].remote_info).toEqual(remote_info);
    expect(result.view.task_references).toEqual(before.task_references);
    expect(result.view.port_forwards).toEqual(before.port_forwards);
  });

  it("rejects a removed method instead of silently trying the preferred method", () => {
    const before = restoreWorkspace(document());
    expect(() => recoverRemoteHost(before, {
      kind: "ssh", destination: "old-ip", host_id: "old", method_id: "removed",
    }, remote_info)).toThrow("connection method is no longer saved");
  });
});

describe("Tailscale account recovery", () => {
  const device: TailscaleDevice = {
    node_id: "builder-node", name: "Builder", dns_name: "builder.tail.example.ts.net",
    addresses: ["100.90.80.70"], online: true, os: "linux",
  };

  function virtualWorkspace() {
    const host_id = tailscaleHostId(device.node_id);
    return restoreWorkspace({
      schema_version: 8,
      workspace_id: "default",
      sessions: [{ host_id, session_id: "shell", name: "Shell", last_known_cwd: null, last_known_cwd_display: null }],
      tabs: [{ kind: "session", host_id, session_id: "shell" }],
      active_tab: { kind: "session", host_id, session_id: "shell" },
    }, { schema_version: 1, hosts: [], ssh_gateways: [] }, [], [device]);
  }

  it("saves an explicitly selected account and the verified settings under the existing host and method identities", () => {
    const before = virtualWorkspace();
    const candidate = {
      ...before.targets[1] as SshConnectionTarget,
      user: "developer", identity_file: "~/.ssh/builder", port: 2222,
    };
    const recovered = recoverRemoteHost(before, candidate, remote_info)!;
    const catalog = hostCatalogDocument(recovered.view);

    expect(recovered.view.hosts[1]).toMatchObject({ source: "saved", remote_info });
    expect(catalog.hosts).toEqual([{
      host_id: candidate.host_id, name: device.name, preferred_method_id: "tailscale", remote_info,
      connection_methods: [{
        method_id: "tailscale", name: "Tailscale", tailscale_node_id: device.node_id,
        target: {
          kind: "ssh", destination: device.dns_name, hostname: device.addresses[0],
          user: "developer", identity_file: "~/.ssh/builder", port: 2222,
        },
      }],
    }]);
    expect(recovered.view.active_tab_key).toBe(before.active_tab_key);
    expect(recovered.view.sessions[0].target).toBe(recovered.target);
    expect(recovered.view.tabs[0].target).toBe(recovered.target);
    expect(recovered.key_changes.size).toBe(0);
    expect(before.hosts[1].source).toBe("tailscale");
    expect(before.hosts[1].connection_methods[0].target).not.toHaveProperty("user");

    const changed_device = { ...device, addresses: ["100.90.80.71"] };
    const reloaded = restoreWorkspace(workspaceDocument(recovered.view), catalog, [], [changed_device]);
    expect(reloaded.hosts).toHaveLength(2);
    expect(reloaded.targets[1]).toMatchObject({
      host_id: candidate.host_id, method_id: "tailscale", tailscale_node_id: device.node_id,
      user: "developer", hostname: changed_device.addresses[0], remote_info,
    });
  });

  it.each([undefined, ""])("keeps the default SSH account virtual (%s)", (user) => {
    const before = virtualWorkspace();
    const recovered = recoverRemoteHost(before, { ...before.targets[1] as SshConnectionTarget, user }, remote_info)!;
    expect(recovered.view.hosts[1]).toMatchObject({ source: "tailscale", remote_info });
    expect(hostCatalogDocument(recovered.view).hosts).toEqual([]);
  });

  it("checks the expected remote identity before saving an account customization", () => {
    const before = virtualWorkspace();
    before.hosts[1].expected_remote_info = remote_info;
    const snapshot = structuredClone(before);
    expect(() => recoverRemoteHost(before, {
      ...before.targets[1] as SshConnectionTarget, user: "another-account",
    }, { ...remote_info, remote_id: "another-environment" })).toThrow("different remote environment");
    expect(before).toEqual(snapshot);
    expect(hostCatalogDocument(before).hosts).toEqual([]);
  });

  it("rejects a removed connection method before saving an account customization", () => {
    const before = virtualWorkspace();
    const snapshot = structuredClone(before);
    expect(() => recoverRemoteHost(before, {
      ...before.targets[1] as SshConnectionTarget, method_id: "removed", user: "developer",
    }, remote_info)).toThrow("connection method is no longer saved");
    expect(before).toEqual(snapshot);
    expect(hostCatalogDocument(before).hosts).toEqual([]);
  });
});

describe("SSH endpoint comparison", () => {
  it("ignores host labels but detects a changed resolved gateway", () => {
    const before: SshConnectionTarget = {
      kind: "ssh",
      destination: "build.internal",
      host_id: "build",
      host_name: "Builder",
      method_id: "office",
      gateway_route: [{ gateway_id: "edge", mode: "native_only" }],
      gateways: [{ gateway_id: "edge", name: "Edge", destination: "edge.old", mode: "native_only" }],
    };

    expect(sameSshEndpoint(before, { ...before, host_name: "Renamed builder" })).toBe(true);
    expect(sameSshEndpoint(before, {
      ...before,
      gateways: [{ ...before.gateways![0], destination: "edge.new" }],
    })).toBe(false);
  });
});
