import { describe, expect, it } from "vitest";
import type {
  RemoteIdentity,
  WorkspaceDocument,
  WorkspaceHost,
  WorkspaceSshGateway,
} from "../../lib/types";
import { sessionKey, targetKey, targetLabel } from "../targets/targets";
import {
  hostFromTarget,
  hostTarget,
  restoreWorkspace,
  updateHostSettings,
  workspaceDocument,
} from "./workspaceModel";

const remote_info: RemoteIdentity = {
  remote_id: "machine-identity",
  agent_version: "0.1.0",
};
const gateway: WorkspaceSshGateway = {
  gateway_id: "edge",
  name: "Edge",
  destination: "edge.example",
};

function host(): WorkspaceHost {
  return {
    host_id: "build-machine",
    name: "Build machine",
    remote_info,
    preferred_method_id: "lan",
    connection_methods: [
      {
        method_id: "lan",
        name: "Office network",
        target: { kind: "ssh", destination: "build.local", user: "alice" },
      },
      {
        method_id: "gateway",
        name: "Via edge",
        target: {
          kind: "ssh",
          destination: "build.internal",
          hostname: "10.0.0.5",
          user: "alice",
          gateway_route: [{ gateway_id: "edge", mode: "native_only" }],
        },
      },
    ],
  };
}

function document(): WorkspaceDocument {
  return {
    schema_version: 7,
    workspace_id: "default",
    hosts: [hostFromTarget({ kind: "local" }), host()],
    ssh_gateways: [gateway],
    sessions: [{
      host_id: "build-machine",
      session_id: "shell",
      name: "Build",
      last_known_cwd: "/work",
      last_known_cwd_display: "~/work",
    }],
    tabs: [{ kind: "session", host_id: "build-machine", session_id: "shell" }],
    active_tab: { kind: "session", host_id: "build-machine", session_id: "shell" },
    task_references: [{
      host_id: "build-machine",
      task_id: "job",
      definition_id: null,
      applied_revision: null,
      is_default: false,
    }],
    port_forwards: [{
      host_id: "build-machine",
      forward_id: "http",
      name: "Preview",
      enabled: true,
      bind_address: "127.0.0.1",
      local_port: 8080,
      remote_host: "127.0.0.1",
      remote_port: 80,
    }],
  };
}

describe("host identity and connection methods", () => {
  it.each([1, 2, 3, 4, 5, 6] as const)("migrates schema %s without changing host or session identity", (schema_version) => {
    const legacy: WorkspaceDocument = {
      ...document(),
      schema_version,
      hosts: [
        { host_id: "local", target: { kind: "local" } },
        {
          host_id: "build-machine",
          target: {
            kind: "ssh",
            destination: "old-alias",
            hostname: "10.0.0.5",
            user: "alice",
            port: 2222,
            identity_file: "~/.ssh/build",
            remote_info,
          },
        },
      ],
    };
    const view = restoreWorkspace(legacy);
    const saved = workspaceDocument(view);

    expect(saved.schema_version).toBe(7);
    expect(saved.hosts[1]).toEqual({
      host_id: "build-machine",
      name: "old-alias",
      remote_info,
      preferred_method_id: "default",
      connection_methods: [{
        method_id: "default",
        name: "SSH",
        target: {
          kind: "ssh",
          destination: "old-alias",
          hostname: "10.0.0.5",
          user: "alice",
          port: 2222,
          identity_file: "~/.ssh/build",
        },
      }],
    });
    expect(saved.sessions).toEqual(legacy.sessions);
    expect(saved.tabs).toEqual(legacy.tabs);
    expect(saved.active_tab).toEqual(legacy.active_tab);
    expect(saved.task_references).toEqual(legacy.task_references);
    expect(saved.port_forwards).toEqual(legacy.port_forwards);
  });

  it("resolves the preferred method by default and another only by explicit selection", () => {
    const saved_host = host();
    const preferred = hostTarget(saved_host, [gateway]);
    const selected = hostTarget(saved_host, [gateway], "gateway");

    expect(preferred).toMatchObject({
      host_id: "build-machine", host_name: "Build machine", method_id: "lan",
      destination: "build.local", remote_info,
    });
    expect(preferred).not.toHaveProperty("gateways");
    expect(selected).toMatchObject({
      host_id: "build-machine", host_name: "Build machine", method_id: "gateway",
      destination: "build.internal", remote_info,
      gateways: [{ ...gateway, mode: "native_only" }],
    });
    expect(targetKey(selected)).toBe(targetKey(preferred));
    expect(saved_host.preferred_method_id).toBe("lan");
    expect(() => hostTarget(saved_host, [gateway], "removed")).toThrow("Choose a connection method");
    expect(() => hostTarget({ ...saved_host, preferred_method_id: "removed" }, [gateway])).toThrow("Choose a connection method");
  });

  it("keeps active transports and references when host settings are renamed or edited", () => {
    const before = restoreWorkspace(document());
    const updated_host = {
      ...before.hosts[1],
      name: "Production builder",
      preferred_method_id: "gateway",
      connection_methods: before.hosts[1].connection_methods.map((method) => method.method_id === "lan"
        ? { ...method, target: { ...method.target, destination: "changed.local" } }
        : method),
    };
    const after = updateHostSettings(before, updated_host);

    expect(after.targets[1]).toMatchObject({
      destination: "build.local", method_id: "lan", host_name: "Production builder",
    });
    expect(after.sessions[0].target).toEqual(after.targets[1]);
    expect(after.tabs[0].target).toEqual(after.targets[1]);
    expect(targetLabel(after.targets[1])).toBe("Production builder");
    expect(sessionKey(after.sessions[0])).toBe(sessionKey(before.sessions[0]));
    expect(after.active_tab_key).toBe(before.active_tab_key);
    expect(after.tab_order).toEqual(before.tab_order);
    expect(after.shell_states).toEqual(before.shell_states);
    expect(after.task_references).toEqual(before.task_references);
    expect(after.port_forwards).toEqual(before.port_forwards);
    expect(before.hosts[1].name).toBe("Build machine");
    expect(workspaceDocument(after).hosts[1]).toEqual(updated_host);
    expect(restoreWorkspace(workspaceDocument(after)).targets[1]).toMatchObject({
      method_id: "gateway", destination: "build.internal", host_name: "Production builder",
    });
  });

  it("persists every method without duplicating runtime identity or resolved gateway details", () => {
    const view = restoreWorkspace(document());
    view.hosts[1].connection_methods[1].target = {
      ...view.hosts[1].connection_methods[1].target,
      host_id: "runtime-host",
      host_name: "Runtime label",
      method_id: "runtime-method",
      remote_info,
      gateways: [{ ...gateway, mode: "native_only" }],
    };

    const saved = workspaceDocument(view);
    expect(saved.hosts[1]).toEqual(host());
    expect(saved.ssh_gateways).toEqual([gateway]);
    expect(restoreWorkspace(saved).hosts).toEqual(saved.hosts);
  });

  it("refuses a missing gateway instead of silently falling back to direct SSH", () => {
    expect(() => hostTarget(host(), [], "gateway")).toThrow("gateway for this connection method is missing");
    expect(() => restoreWorkspace({
      ...document(),
      hosts: [{ ...host(), preferred_method_id: "gateway" }],
      ssh_gateways: [],
    })).toThrow("gateway for this connection method is missing");
  });

  it("rejects settings changes for a removed host", () => {
    const view = restoreWorkspace(document());
    expect(() => updateHostSettings(view, { ...host(), host_id: "removed" }))
      .toThrow("no longer in the workspace");
  });
});
