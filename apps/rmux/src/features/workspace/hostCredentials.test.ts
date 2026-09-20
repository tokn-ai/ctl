import { describe, expect, it } from "vitest";
import type { ResolvedSshGateway, SshConnectionTarget, WorkspaceHost } from "../../lib/types";
import { removableHostCredentials } from "./hostCredentials";
import { emptyWorkspaceView, hostTarget } from "./workspaceModel";

function host(host_id: string, target: SshConnectionTarget): WorkspaceHost {
  return {
    host_id, name: host_id, preferred_method_id: "ssh",
    connection_methods: [{ method_id: "ssh", name: "SSH", target }],
  };
}

const gateway: ResolvedSshGateway = {
  gateway_id: "edge", name: "Edge", destination: "edge.example", hostname: "10.0.0.1",
  user: "deploy", port: 2222, identity_file: "~/.ssh/edge", mode: "native_only",
};

describe("host credential cleanup", () => {
  it("retains a shared direct alias even when transport overrides differ", () => {
    const view = emptyWorkspaceView();
    view.hosts.push(
      host("removed", { kind: "ssh", destination: "build", hostname: "10.0.0.8", user: "deploy" }),
      host("kept", { kind: "ssh", destination: "build", user: "operator", port: 2222, identity_file: "~/.ssh/other" }),
    );
    expect(removableHostCredentials(view, "removed")).toEqual([]);
  });

  it("uses gateway transport settings, excluding names and gateway IDs, to match scopes", () => {
    const view = emptyWorkspaceView();
    view.hosts.push(host("removed", { kind: "ssh", destination: "build" }));
    const routed = { kind: "ssh" as const, host_id: "removed", destination: "build", gateways: [gateway] };
    view.targets.push(routed, {
      ...routed, host_id: "kept", gateways: [{ ...gateway, gateway_id: "renamed", name: "Office edge" }],
    });
    // The shared gateway scope is retained; the separate direct scope is removed.
    expect(removableHostCredentials(view, "removed")).toEqual([hostTarget(view.hosts[1], [])]);
    view.targets[2] = { ...routed, host_id: "kept", gateways: [{ ...gateway, port: 2223 }] };
    expect(removableHostCredentials(view, "removed")).toHaveLength(2);
  });

  it("cleans up obsolete runtime routes once while protecting routes used by another live attachment", () => {
    const view = emptyWorkspaceView();
    view.hosts.push(host("removed", { kind: "ssh", destination: "new-route" }));
    const old: SshConnectionTarget = { kind: "ssh", host_id: "removed", destination: "old-route" };
    view.targets.push(old);
    expect(removableHostCredentials(view, "removed", old).map((target) => target.destination))
      .toEqual(["new-route", "old-route"]);
    expect(removableHostCredentials(view, "removed", { ...old, host_id: "kept" }).map((target) => target.destination))
      .toEqual(["new-route"]);
  });

  it("resolves saved gateway chains before comparing them to runtime snapshots", () => {
    const view = emptyWorkspaceView();
    view.ssh_gateways = [gateway];
    view.hosts.push(host("removed", {
      kind: "ssh", destination: "build", gateway_route: [{ gateway_id: "edge", mode: "native_only" }],
    }));
    view.targets.push({ kind: "ssh", host_id: "kept", destination: "build", gateways: [gateway] });
    expect(removableHostCredentials(view, "removed")).toEqual([]);
  });
});
