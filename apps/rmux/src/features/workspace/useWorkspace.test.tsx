// @vitest-environment jsdom
import { StrictMode } from "react";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkspace } from "./useWorkspace";
import { emptyWorkspaceView, workspaceDocument } from "./workspaceModel";
import type { WorkspaceDocument, WorkspaceSnapshot } from "../../lib/types";

const api = vi.hoisted(() => ({
  loadWorkspace: vi.fn(),
  updateWorkspace: vi.fn(),
}));
vi.mock("../../lib/tauri", () => api);

const initial: WorkspaceSnapshot = {
  revision: "one",
  document: {
    schema_version: 1,
    workspace_id: "default",
    hosts: [
      { host_id: "local", target: { kind: "local" } },
      { host_id: "server", target: { kind: "ssh", destination: "server" } },
    ],
    sessions: [
      {
        host_id: "server",
        session_id: "shell",
        name: "remembered",
        last_known_cwd: "/work",
        last_known_cwd_display: "~/work",
      },
    ],
    tabs: [{ host_id: "server", session_id: "shell" }],
    active_tab: { host_id: "server", session_id: "shell" },
  },
};

beforeEach(() => {
  vi.resetAllMocks();
  window.localStorage.clear();
  api.loadWorkspace.mockResolvedValue(initial);
  api.updateWorkspace.mockImplementation(
    async (_revision: string | null, document: WorkspaceDocument) => ({
      revision: crypto.randomUUID(),
      document,
    }),
  );
});
afterEach(cleanup);

describe("workspace lifecycle", () => {
  it("loads remembered sessions and tabs without any transport operations or writes", async () => {
    const { result } = renderHook(() => useWorkspace(), {
      wrapper: StrictMode,
    });
    await waitFor(() => expect(result.current.ready).toBe(true));
    expect(result.current.sessions[0].status).toBe("unknown");
    expect(result.current.tabs[0].name).toBe("remembered");
    expect(api.updateWorkspace).not.toHaveBeenCalled();
  });

  it("migrates hosts once, and removes legacy storage only after the save succeeds", async () => {
    const legacy = JSON.stringify({
      schema_version: 1,
      ssh_destinations: ["legacy"],
    });
    window.localStorage.setItem("rmux.remote_hosts", legacy);
    api.loadWorkspace.mockResolvedValue({
      revision: null,
      document: workspaceDocument(emptyWorkspaceView()),
    });
    let complete!: (snapshot: WorkspaceSnapshot) => void;
    api.updateWorkspace.mockImplementation(
      () =>
        new Promise((resolve) => {
          complete = resolve;
        }),
    );
    const { result } = renderHook(() => useWorkspace(), {
      wrapper: StrictMode,
    });
    await waitFor(() => expect(api.updateWorkspace).toHaveBeenCalledTimes(1));
    expect(window.localStorage.getItem("rmux.remote_hosts")).toBe(legacy);
    expect(result.current.ready).toBe(false);
    const document = api.updateWorkspace.mock.calls[0][1];
    await act(async () => complete({ revision: "migrated", document }));
    expect(result.current.targets).toHaveLength(2);
    expect(window.localStorage.getItem("rmux.remote_hosts")).toBeNull();
  });

  it("preserves the legacy copy when migration fails and never replaces an unreadable workspace", async () => {
    window.localStorage.setItem(
      "rmux.remote_hosts",
      JSON.stringify({ schema_version: 1, ssh_destinations: ["legacy"] }),
    );
    api.loadWorkspace.mockResolvedValue({
      revision: null,
      document: workspaceDocument(emptyWorkspaceView()),
    });
    api.updateWorkspace.mockRejectedValue({
      code: "workspace_io_failed",
      message: "disk full",
    });
    const first = renderHook(() => useWorkspace());
    await waitFor(() =>
      expect(first.result.current.error).toContain("disk full"),
    );
    expect(first.result.current.ready).toBe(false);
    expect(window.localStorage.getItem("rmux.remote_hosts")).not.toBeNull();
    first.unmount();
    api.updateWorkspace.mockClear();
    api.loadWorkspace.mockRejectedValue({
      code: "workspace_unreadable",
      message: "preserved",
    });
    const second = renderHook(() => useWorkspace());
    await waitFor(() => expect(second.result.current.error).toBe("preserved"));
    expect(api.updateWorkspace).not.toHaveBeenCalled();
  });

  it("persists membership independently of tabs, and exposes save failures", async () => {
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    await act(async () => {
      result.current.setTabs([]);
      await result.current.persist();
    });
    const saved =
      api.updateWorkspace.mock.calls[
        api.updateWorkspace.mock.calls.length - 1
      ][1];
    expect(saved.sessions).toHaveLength(1);
    expect(saved.tabs).toEqual([]);
    api.updateWorkspace.mockRejectedValue({
      code: "workspace_io_failed",
      message: "disk full",
    });
    await act(async () => {
      result.current.setSessions([]);
    });
    await waitFor(() => expect(result.current.error).toBe("disk full"));
    expect(result.current.sessions).toEqual([]);
  });
});
