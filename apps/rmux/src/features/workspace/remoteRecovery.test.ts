import { describe, expect, it } from "vitest";
import { recoverRemoteHost } from "./remoteRecovery";
import { restoreWorkspace, workspaceDocument } from "./workspaceModel";
import type { RemoteIdentity, WorkspaceDocument } from "../../lib/types";

const remote_info: RemoteIdentity = { remote_id: "b765c444-28d0-4772-bb16-d3b212290bcb", agent_version: "0.1.0" };
function document(): WorkspaceDocument {
  return {
    schema_version: 3, workspace_id: "default",
    hosts: [
      { host_id: "local", target: { kind: "local" } },
      { host_id: "old", target: { kind: "ssh", destination: "old-ip", user: "alice", remote_info } },
    ],
    sessions: [{ host_id: "old", session_id: "shell", name: "work", last_known_cwd: "/work", last_known_cwd_display: "~/work" }],
    tabs: [{ host_id: "old", session_id: "shell" }],
    active_tab: { host_id: "old", session_id: "shell" },
  };
}

describe("remote environment recovery", () => {
  it("rebinds a new address while preserving IDs, tabs, cached shell metadata, and version across reload", () => {
    const before = restoreWorkspace(document());
    const identity = { ...remote_info, agent_version: "0.2.0" };
    const recovered = recoverRemoteHost(before, { kind: "ssh", destination: "new-name", hostname: "10.0.0.5", user: "alice" }, identity)!;
    expect(recovered.target).toMatchObject({ host_id: "old", hostname: "10.0.0.5", remote_info: identity });
    expect(recovered.view.active_tab_key).toBe(before.active_tab_key);
    expect(recovered.view.tab_order).toEqual(before.tab_order);
    expect(recovered.view.shell_states).toEqual(before.shell_states);
    expect(recovered.view.tabs[0].target).toBe(recovered.target);
    expect(recovered.view.sessions[0].target).toBe(recovered.target);
    expect(restoreWorkspace(workspaceDocument(recovered.view)).targets).toEqual(recovered.view.targets);
    expect(before.targets[1]).toMatchObject({ destination: "old-ip" });
  });

  it("merges a previously saved alias without losing its distinct tabs or duplicating sessions and task references", () => {
    const saved = document();
    saved.hosts.push({ host_id: "alias", target: { kind: "ssh", destination: "new-ip" } });
    saved.sessions.push(...["shell", "other"].map((session_id) => ({ ...saved.sessions[0], host_id: "alias", session_id })));
    saved.tabs.push({ host_id: "alias", session_id: "shell" }, { host_id: "alias", session_id: "other" });
    saved.active_tab = saved.tabs[2];
    saved.task_references = ["old", "alias"].map((host_id) => ({ host_id, task_id: "job", definition_id: null, applied_revision: null, is_default: false }));
    saved.tabs.push({ kind: "task", host_id: "alias", task_id: "job" });
    const recovered = recoverRemoteHost(restoreWorkspace(saved), { kind: "ssh", destination: "new-ip", host_id: "alias" }, remote_info)!;
    const persisted = workspaceDocument(recovered.view);
    expect(persisted.hosts).toHaveLength(2);
    expect(persisted.sessions.map((session) => [session.host_id, session.session_id])).toEqual([["old", "shell"], ["old", "other"]]);
    expect(persisted.tabs).toHaveLength(3);
    expect(persisted.active_tab).toEqual({ kind: "session", host_id: "old", session_id: "other" });
    expect(persisted.task_references).toHaveLength(1);
    expect(persisted.tabs[2]).toEqual({ kind: "task", host_id: "old", task_id: "job" });
  });

  it("keeps a different environment separate and rejects an address collision", () => {
    const before = restoreWorkspace(document());
    const other = { ...remote_info, remote_id: "2c07a8fa-0c24-452c-bbbb-8a57fe89a035" };
    expect(recoverRemoteHost(before, { kind: "ssh", destination: "other" }, other)).toBeNull();
    expect(() => recoverRemoteHost(before, { kind: "ssh", destination: "old-ip" }, other)).toThrow("different saved remote environment");
  });

  it("learns an existing legacy host without changing its references", () => {
    const saved = document();
    saved.hosts[1].target = { kind: "ssh", destination: "old-ip" };
    const before = restoreWorkspace(saved);
    const result = recoverRemoteHost(before, { kind: "ssh", destination: "old-ip", host_id: "old" }, remote_info)!;
    expect(result.view.active_tab_key).toBe(before.active_tab_key);
    expect(result.target.remote_info).toEqual(remote_info);
  });
});
