import { describe, expect, it, vi } from "vitest";
import { WorkspaceWriter } from "./WorkspaceWriter";
import {
  emptyWorkspaceView,
  restoreWorkspace,
  withHostId,
  workspaceDocument,
} from "./workspaceModel";
import { sessionKey, targetKey } from "../targets/targets";
import type { WorkspaceDocument, WorkspaceSnapshot } from "../../lib/types";

export function savedWorkspace(): WorkspaceSnapshot {
  return {
    revision: "initial",
    document: {
      schema_version: 1,
      workspace_id: "default",
      hosts: [
        { host_id: "local", target: { kind: "local" } },
        { host_id: "remote", target: { kind: "ssh", destination: "test" } },
        { host_id: "unused", target: { kind: "ssh", destination: "unused" } },
      ],
      sessions: [
        {
          host_id: "remote",
          session_id: "first",
          name: "first",
          last_known_cwd: "/work",
          last_known_cwd_display: "~/work",
        },
        {
          host_id: "local",
          session_id: "second",
          name: "second",
          last_known_cwd: null,
          last_known_cwd_display: null,
        },
      ],
      tabs: [
        { host_id: "local", session_id: "second" },
        { host_id: "remote", session_id: "first" },
      ],
      active_tab: { host_id: "remote", session_id: "first" },
    },
  };
}

describe("workspace model", () => {
  it("restores tab order and stable references, with unverified runtime state", () => {
    const snapshot = savedWorkspace();
    const view = restoreWorkspace(snapshot.document);
    expect(view.tabs.map((tab) => tab.session_id)).toEqual(["second", "first"]);
    expect(view.sessions.every((session) => session.status === "unknown")).toBe(
      true,
    );
    expect(view.active_tab_key).toBe(sessionKey(view.sessions[0]));
    expect(workspaceDocument(view)).toEqual({
      ...snapshot.document,
      schema_version: 3,
      task_definition_scope: { kind: "global" },
      task_references: [],
      task_drafts: [],
      sidebar_view: "sessions",
      tabs: snapshot.document.tabs.map((tab) => ({ ...tab, kind: "session" })),
      active_tab: { ...snapshot.document.active_tab, kind: "session" },
    });
  });

  it("retains incomplete drafts and sidebar selection while migrating definition tabs", () => {
    const document = savedWorkspace().document;
    const definition = {
      name: "Half written",
      program: "",
      arguments: [""],
      working_directory: "relative/incomplete",
      execution_mode: "background" as const,
    };
    document.task_drafts = [{ definition_id: "draft-id", definition }];
    document.task_definitions = [
      {
        definition_id: "saved-id",
        revision: "r1",
        definition: { ...definition, program: "cargo" },
      },
    ];
    document.tabs.push({ kind: "task_definition", definition_id: "saved-id" });
    document.active_tab = document.tabs[2];
    document.sidebar_view = "tasks";
    const view = restoreWorkspace(document);
    const persisted = workspaceDocument(view);
    expect(view.task_tabs).toEqual([]);
    expect(persisted.tabs).toHaveLength(2);
    expect(persisted.active_tab).toEqual(persisted.tabs[0]);
    expect(persisted.task_drafts).toEqual(document.task_drafts);
    expect(persisted.sidebar_view).toBe("tasks");
    expect(view.task_definitions).toEqual(document.task_definitions);
    expect(persisted.task_definitions).toBeUndefined();
  });

  it("keeps external definition references and source scopes without rewriting the catalog", () => {
    const view = emptyWorkspaceView();
    const scope = { kind: "project" as const, project_root: "/work/project" };
    view.task_definition_scope = scope;
    view.task_references = [{ host_id: "local", task_id: "task", definition_id: "external", definition_scope: scope, applied_revision: "r1", is_default: true }];
    view.task_drafts = [{ definition_id: "external", scope, base_revision: "r1", definition: { name: "build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" } }];
    const document = workspaceDocument(view);
    expect(document.task_definitions).toBeUndefined();
    expect(document.task_references).toEqual(view.task_references);
    expect(document.task_drafts).toEqual(view.task_drafts);
    expect(restoreWorkspace(document).task_definition_scope).toEqual(scope);
  });

  it("never persists runtime status, output positions, commands, or credentials", () => {
    const view = restoreWorkspace(savedWorkspace().document);
    view.sessions[0].status = "running";
    view.sessions[0].next_sequence = "9999999";
    view.shell_states = new Map([
      [
        sessionKey(view.sessions[0]),
        {
          shell_type: "zsh",
          cwd: "/new",
          cwd_display: "~/new",
          running_command: "sensitive argument",
          prompt_phase: "running",
          tui_hint: "unknown",
          revision: "8",
          observed_sequence: "9",
        },
      ],
    ]);
    const saved = workspaceDocument(view);
    expect(saved.sessions[0].last_known_cwd).toBe("/new");
    const encoded = JSON.stringify(saved);
    for (const field of [
      "running",
      "next_sequence",
      "9999999",
      "sensitive",
      "password",
      "attachment_token",
    ]) {
      expect(encoded).not.toContain(field);
    }
  });

  it("keeps host identity stable across alias changes and drops orphaned tabs", () => {
    const target = withHostId({ kind: "ssh", destination: "before" });
    expect(target.kind).toBe("ssh");
    if (target.kind !== "ssh") throw new Error("Expected SSH");
    expect(targetKey({ ...target, destination: "after" })).toBe(
      targetKey(target),
    );
    const view = restoreWorkspace(savedWorkspace().document);
    view.targets = [{ kind: "local" }];
    const saved = workspaceDocument(view);
    expect(saved.sessions).toHaveLength(1);
    expect(saved.tabs).toHaveLength(1);
    expect(saved.active_tab).toBeNull();
  });
});

describe("workspace writer", () => {
  it("serializes revisions and skips identical metadata even when runtime changes", async () => {
    const save = vi.fn(
      async (revision: string | null, document: WorkspaceDocument) => ({
        revision: `${revision}-next`,
        document,
      }),
    );
    const writer = new WorkspaceWriter(savedWorkspace(), save);
    const view = restoreWorkspace(savedWorkspace().document);
    view.tabs = [];
    const first = writer.write(workspaceDocument(view));
    view.sessions = [];
    const second = writer.write(workspaceDocument(view));
    await Promise.all([first, second]);
    expect(save.mock.calls.map(([revision]) => revision)).toEqual([
      "initial",
      "initial-next",
    ]);
    await writer.write(workspaceDocument(view));
    expect(save).toHaveBeenCalledTimes(2);
  });

  it("retries an I/O failure but never overwrites another writer after a conflict", async () => {
    const save = vi
      .fn()
      .mockRejectedValueOnce({
        code: "workspace_io_failed",
        message: "disk full",
      })
      .mockResolvedValueOnce({
        revision: "saved",
        document: workspaceDocument(emptyWorkspaceView()),
      })
      .mockRejectedValue({
        code: "workspace_conflict",
        message: "another writer",
      });
    const writer = new WorkspaceWriter(savedWorkspace(), save);
    const document = workspaceDocument(emptyWorkspaceView());
    await expect(writer.write(document)).rejects.toMatchObject({
      code: "workspace_io_failed",
    });
    await writer.write(document, true);
    const changed = { ...document, workspace_id: "changed" };
    await expect(writer.write(changed)).rejects.toMatchObject({
      code: "workspace_conflict",
    });
    await expect(writer.write(changed, true)).rejects.toMatchObject({
      code: "workspace_conflict",
    });
    expect(save).toHaveBeenCalledTimes(3);
  });
});
