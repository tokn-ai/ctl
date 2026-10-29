import { describe, expect, it } from "vitest";
import type { SessionSummary } from "../../lib/types";
import { sessionKey } from "../targets/targets";
import {
  emptyWorkspaceView,
  restoreWorkspace,
  workspaceDocument,
  workspaceTabKey,
  type WorkspaceView,
} from "./workspaceModel";
import { openWorkspaceSessionTab } from "./workspaceTabs";

function session(session_id: string): SessionSummary {
  return {
    target: { kind: "local" },
    session_id,
    name: session_id,
    status: "running",
    terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
    next_sequence: "0",
  };
}

function workspace(): WorkspaceView {
  const tabs = [session("first"), session("second"), session("third")];
  const task_tabs = [
    { kind: "task" as const, host_id: "local", task_id: "build" },
    { kind: "task" as const, host_id: "local", task_id: "watch" },
  ];
  return {
    ...emptyWorkspaceView(),
    sessions: [...tabs, session("created")],
    tabs,
    task_tabs,
    task_references: task_tabs.map((tab) => ({
      host_id: tab.host_id,
      task_id: tab.task_id,
      definition_id: null,
      applied_revision: null,
      is_default: false,
    })),
    tab_order: [
      sessionKey(tabs[0]), workspaceTabKey(task_tabs[0]), sessionKey(tabs[1]),
      workspaceTabKey(task_tabs[1]), sessionKey(tabs[2]),
    ],
    active_tab_key: sessionKey(tabs[1]),
  };
}

describe("workspace session tab opening", () => {
  it("inserts after the source across interleaved tasks and persists that order", () => {
    const before = workspace();
    const created = before.sessions[3];
    const next = openWorkspaceSessionTab(before, created, sessionKey(before.tabs[1]));

    expect(next.tabs.map((tab) => tab.session_id)).toEqual(["first", "second", "created", "third"]);
    expect(next.tab_order).toEqual([
      ...before.tab_order.slice(0, 3), sessionKey(created), ...before.tab_order.slice(3),
    ]);
    expect(next.active_tab_key).toBe(sessionKey(created));
    expect(next.task_tabs).toBe(before.task_tabs);
    expect(next.sessions).toBe(before.sessions);

    const saved = workspaceDocument(next);
    expect(saved.tabs.map(workspaceTabKey)).toEqual(next.tab_order);
    expect(restoreWorkspace(saved).tab_order).toEqual(next.tab_order);
    expect(before.tabs.map((tab) => tab.session_id)).toEqual(["first", "second", "third"]);
    expect(before.tab_order).toEqual(workspace().tab_order);
    expect(before.active_tab_key).toBe(sessionKey(before.tabs[1]));
  });

  it("selects and updates an existing tab without moving it or surrounding tasks", () => {
    const before = workspace();
    const updated = { ...before.tabs[0], name: "renamed" };
    const next = openWorkspaceSessionTab(before, updated, sessionKey(before.tabs[1]));

    expect(next.tab_order).toEqual(before.tab_order);
    expect(next.tabs).toEqual([updated, ...before.tabs.slice(1)]);
    expect(next.active_tab_key).toBe(sessionKey(updated));
    expect(before.tabs[0].name).toBe("first");
  });

  it.each([undefined, "closed-session"])("appends safely when the anchor is %s", (after_tab_key) => {
    const before = workspace();
    const created = before.sessions[3];
    const next = openWorkspaceSessionTab(before, created, after_tab_key);

    expect(next.tabs).toEqual([...before.tabs, created]);
    expect(next.tab_order).toEqual([...before.tab_order, sessionKey(created)]);
  });

  it("discards a closed anchor from the unified order before placing the new tab", () => {
    const before = workspace();
    const anchor = sessionKey(before.tabs[1]);
    before.tabs = before.tabs.filter((tab) => sessionKey(tab) !== anchor);
    const created = before.sessions[3];
    const next = openWorkspaceSessionTab(before, created, anchor);

    expect(next.tab_order).toEqual([
      ...before.tab_order.filter((key) => key !== anchor), sessionKey(created),
    ]);
    expect(next.tabs).toEqual([...before.tabs, created]);
  });
});
