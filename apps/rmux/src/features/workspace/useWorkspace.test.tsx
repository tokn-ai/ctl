// @vitest-environment jsdom
import { StrictMode } from "react";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkspace } from "./useWorkspace";
import {
  emptyWorkspaceView,
  projectedHostId,
  promoteHost,
  updateHostSettings,
  workspaceDocument,
} from "./workspaceModel";
import { recoverRemoteHost } from "./remoteRecovery";
import type { HostCatalogDocument, HostCatalogSnapshot, WorkspaceDocument, WorkspaceSnapshot } from "../../lib/types";

const api = vi.hoisted(() => ({
  loadWorkspace: vi.fn(),
  updateWorkspace: vi.fn(),
  loadHosts: vi.fn(),
  updateHosts: vi.fn(),
  listSshConfigHosts: vi.fn(),
}));
const nativeWindow = vi.hoisted(() => ({
  onCloseRequested: vi.fn(),
  destroy: vi.fn(),
}));
vi.mock("../../lib/tauri", () => api);
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => nativeWindow,
}));

const initial: WorkspaceSnapshot = {
  revision: "one",
  document: {
    schema_version: 8,
    workspace_id: "default",
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

const initialHosts: HostCatalogSnapshot = {
  revision: "hosts-one",
  document: {
    schema_version: 1,
    hosts: [{
      host_id: "server",
      name: "server",
      preferred_method_id: "default",
      connection_methods: [{ method_id: "default", name: "SSH", target: { kind: "ssh", destination: "server" } }],
    }],
    ssh_gateways: [],
  },
};

const emptyHosts: HostCatalogSnapshot = {
  revision: null,
  document: { schema_version: 1, hosts: [], ssh_gateways: [] },
};

beforeEach(() => {
  vi.resetAllMocks();
  window.localStorage.clear();
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  nativeWindow.onCloseRequested.mockResolvedValue(() => {});
  nativeWindow.destroy.mockResolvedValue(undefined);
  api.loadWorkspace.mockResolvedValue(initial);
  api.loadHosts.mockResolvedValue(initialHosts);
  api.listSshConfigHosts.mockResolvedValue({ hosts: [], warnings: [] });
  api.updateWorkspace.mockImplementation(
    async (_revision: string | null, document: WorkspaceDocument) => ({
      revision: crypto.randomUUID(),
      document,
    }),
  );
  api.updateHosts.mockImplementation(
    async (_revision: string | null, document: HostCatalogDocument) => ({
      revision: crypto.randomUUID(),
      document,
    }),
  );
});
afterEach(() => {
  cleanup();
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

function projectSshHost(alias = "build") {
  const host_id = projectedHostId(alias);
  api.loadHosts.mockResolvedValue(emptyHosts);
  api.listSshConfigHosts.mockResolvedValue({ hosts: [{ destination: alias }], warnings: [] });
  api.loadWorkspace.mockResolvedValue({
    revision: "workspace-one",
    document: {
      ...initial.document,
      sessions: initial.document.sessions.map((session) => ({ ...session, host_id })),
      tabs: [{ host_id, session_id: "shell" }],
      active_tab: { host_id, session_id: "shell" },
    },
  });
  return host_id;
}

describe("workspace lifecycle", () => {
  it("loads remembered sessions and tabs without any transport operations or writes", async () => {
    const { result } = renderHook(() => useWorkspace(), {
      wrapper: StrictMode,
    });
    await waitFor(() => expect(result.current.ready).toBe(true));
    expect(result.current.sessions[0].status).toBe("unknown");
    expect(result.current.tabs[0].name).toBe("remembered");
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
  });

  it("projects SSH config aliases on first load without creating either file", async () => {
    api.loadWorkspace.mockResolvedValue({ revision: null, document: workspaceDocument(emptyWorkspaceView()) });
    api.loadHosts.mockResolvedValue(emptyHosts);
    api.listSshConfigHosts.mockResolvedValue({ hosts: [{ destination: "build" }], warnings: [] });
    const { result } = renderHook(() => useWorkspace(), { wrapper: StrictMode });
    await waitFor(() => expect(result.current.ready).toBe(true));
    expect(result.current.hosts).toHaveLength(2);
    expect(result.current.hosts[1]).toMatchObject({
      host_id: projectedHostId("build"), name: "build", source: "ssh_config",
      connection_methods: [{ target: { kind: "ssh", destination: "build" } }],
    });
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
  });

  it("records verified projected-host observations in the workspace without persisting its definition", async () => {
    const host_id = projectSshHost();
    const remote_info = { remote_id: "verified-build", agent_version: "0.1.0" };
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    await act(async () => result.current.replaceView((current) => {
      const candidate = current.targets.find((target) => target.kind === "ssh" && target.host_id === host_id);
      if (candidate?.kind !== "ssh") throw new Error("Missing projected SSH host");
      return recoverRemoteHost(current, candidate, remote_info)!.view;
    }));
    expect(result.current.hosts[1]).toMatchObject({ source: "ssh_config", remote_info });
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).toHaveBeenCalledOnce();
    expect(api.updateWorkspace.mock.calls[0][1]).toMatchObject({ host_identities: [{ host_id, remote_info }] });
    expect(api.updateWorkspace.mock.calls[0][1].hosts).toBeUndefined();
  });

  it("promotes a customized SSH config host once and retains its session identity", async () => {
    const host_id = projectSshHost();
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    await act(async () => result.current.replaceView((current) => updateHostSettings(current, promoteHost({
      ...current.hosts[1], name: "Build machine",
    }))));
    expect(result.current.hosts).toHaveLength(2);
    expect(result.current.hosts[1]).toMatchObject({ host_id, name: "Build machine", source: "saved" });
    expect(result.current.sessions[0].target).toMatchObject({ host_id, host_name: "Build machine", destination: "build" });
    expect(api.updateHosts).toHaveBeenCalledExactlyOnceWith(null, {
      schema_version: 1,
      hosts: [{
        host_id, name: "Build machine", preferred_method_id: "ssh_config",
        connection_methods: [{ method_id: "ssh_config", name: "SSH config", target: { kind: "ssh", destination: "build" } }],
      }],
      ssh_gateways: [],
    });
    await act(async () => {
      result.current.update("sidebar_view", "ports");
      await result.current.persist();
    });
    expect(api.updateHosts).toHaveBeenCalledOnce();
    const document = api.updateWorkspace.mock.calls.slice(-1)[0][1];
    expect(document.sessions[0].host_id).toBe(host_id);
    expect(document.hosts).toBeUndefined();
  });

  it("finishes a staged promotion before applying discovery that removes its SSH alias", async () => {
    const host_id = projectSshHost();
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    let finish!: (snapshot: HostCatalogSnapshot) => void;
    api.updateHosts.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    let saved!: Promise<void>;
    act(() => {
      saved = result.current.replaceView((current) => updateHostSettings(current, promoteHost({
        ...current.hosts.find((host) => host.host_id === host_id)!, name: "Build machine",
      })));
    });
    await waitFor(() => expect(api.updateHosts).toHaveBeenCalledOnce());
    api.listSshConfigHosts.mockResolvedValue({ hosts: [], warnings: [] });
    let refreshed!: Promise<void>;
    await act(async () => {
      refreshed = result.current.refreshSshConfig();
      await Promise.resolve();
    });
    expect(api.listSshConfigHosts).toHaveBeenCalledTimes(2);
    expect(result.current.hosts.find((host) => host.host_id === host_id)?.source).toBe("ssh_config");
    await act(async () => {
      finish({ revision: "promoted", document: api.updateHosts.mock.calls[0][1] });
      await saved;
      await refreshed;
      await result.current.persist();
    });
    const host = result.current.hosts.find((host) => host.host_id === host_id);
    expect(host).toMatchObject({ host_id, name: "Build machine", source: "saved" });
    expect(host?.connection_methods[0].target.unavailable).toContain("SSH config alias build is missing");
    expect(result.current.sessions[0].target).toMatchObject({
      host_id, host_name: "Build machine", unavailable: expect.stringContaining("SSH config alias build is missing"),
    });
    expect(result.current.error).toBeNull();
    expect(api.updateHosts).toHaveBeenCalledOnce();
    expect(api.updateHosts.mock.calls[0][1].hosts).toHaveLength(1);
    expect(api.updateHosts.mock.calls[0][1].hosts[0].host_id).toBe(host_id);
  });

  it.each(["workspace update", "projected host verification"])("does not rewrite native catalog key order during %s", async (operation) => {
    const host_id = projectSshHost();
    const saved_host = initialHosts.document.hosts[0];
    api.loadHosts.mockResolvedValue({
      revision: "native-order",
      document: {
        schema_version: 1,
        hosts: [{
          host_id: saved_host.host_id,
          name: saved_host.name,
          connection_methods: saved_host.connection_methods,
          preferred_method_id: saved_host.preferred_method_id,
          remote_info: { remote_id: "saved-server", agent_version: "0.1.0" },
        }],
        ssh_gateways: [],
      },
    });
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    await act(async () => {
      if (operation === "workspace update") {
        result.current.update("sidebar_view", "ports");
        await result.current.persist();
      } else {
        await result.current.replaceView((current) => {
          const candidate = current.targets.find((target) => target.kind === "ssh" && target.host_id === host_id);
          if (candidate?.kind !== "ssh") throw new Error("Missing projected SSH host");
          return recoverRemoteHost(current, candidate, { remote_id: "verified-build", agent_version: "0.1.0" })!.view;
        });
      }
    });
    expect(api.updateWorkspace).toHaveBeenCalledOnce();
    expect(api.updateHosts).not.toHaveBeenCalled();
  });

  it("keeps a failed projection customization in memory as projected and retries without duplication", async () => {
    const host_id = projectSshHost();
    api.updateHosts.mockRejectedValueOnce(new Error("disk full"));
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    const customize = () => result.current.replaceView((current) => updateHostSettings(current, promoteHost({
      ...current.hosts.find((host) => host.host_id === host_id)!, name: "Build machine",
    })));
    await act(async () => { await expect(customize()).rejects.toThrow("disk full"); });
    expect(result.current.hosts[1]).toMatchObject({ host_id, name: "build", source: "ssh_config" });
    expect(result.current.error).toBe("disk full");
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    await act(async () => { await customize(); });
    expect(result.current.hosts).toHaveLength(2);
    expect(result.current.hosts[1]).toMatchObject({ host_id, name: "Build machine", source: "saved" });
    expect(api.updateHosts).toHaveBeenCalledTimes(2);
    expect(api.updateHosts.mock.calls[1][1].hosts).toHaveLength(1);
    expect(api.updateHosts.mock.calls[1][1]).toEqual(api.updateHosts.mock.calls[0][1]);
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
    api.loadHosts.mockResolvedValue(emptyHosts);
    let complete!: (snapshot: HostCatalogSnapshot) => void;
    api.updateHosts.mockImplementation(
      () =>
        new Promise((resolve) => {
          complete = resolve;
        }),
    );
    const { result } = renderHook(() => useWorkspace(), {
      wrapper: StrictMode,
    });
    await waitFor(() => expect(api.updateHosts).toHaveBeenCalledTimes(1));
    expect(window.localStorage.getItem("rmux.remote_hosts")).toBe(legacy);
    expect(result.current.ready).toBe(false);
    const document = api.updateHosts.mock.calls[0][1];
    await act(async () => complete({ revision: "migrated", document }));
    await waitFor(() => expect(result.current.ready).toBe(true));
    expect(result.current.targets).toHaveLength(2);
    expect(document.hosts).toHaveLength(1);
    expect(document.hosts[0].connection_methods[0].target.destination).toBe("legacy");
    expect(api.updateWorkspace.mock.calls[0][1].hosts).toBeUndefined();
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
    api.loadHosts.mockResolvedValue(emptyHosts);
    api.updateHosts.mockRejectedValue({
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
    api.updateHosts.mockClear();
    api.loadWorkspace.mockRejectedValue({
      code: "workspace_unreadable",
      message: "preserved",
    });
    const second = renderHook(() => useWorkspace());
    await waitFor(() => expect(second.result.current.error).toBe("preserved"));
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
  });

  it("reuses migrated hosts after a workspace failure instead of importing duplicates on reload", async () => {
    const legacy = JSON.stringify({ schema_version: 1, ssh_destinations: ["legacy"] });
    window.localStorage.setItem("rmux.remote_hosts", legacy);
    api.loadWorkspace.mockResolvedValue({ revision: null, document: workspaceDocument(emptyWorkspaceView()) });
    let catalog = emptyHosts;
    api.loadHosts.mockImplementation(async () => catalog);
    api.updateHosts.mockImplementation(async (_revision: string | null, document: HostCatalogDocument) => {
      catalog = { revision: "hosts-migrated", document };
      return catalog;
    });
    api.updateWorkspace.mockRejectedValueOnce(new Error("workspace disk full"));
    const first = renderHook(() => useWorkspace());
    await waitFor(() => expect(first.result.current.error).toBe("workspace disk full"));
    expect(first.result.current.ready).toBe(false);
    expect(catalog.document.hosts).toHaveLength(1);
    expect(window.localStorage.getItem("rmux.remote_hosts")).toBe(legacy);
    const migrated_id = catalog.document.hosts[0].host_id;
    first.unmount();
    const second = renderHook(() => useWorkspace());
    await waitFor(() => expect(second.result.current.ready).toBe(true));
    expect(second.result.current.hosts).toHaveLength(2);
    expect(second.result.current.hosts[1].host_id).toBe(migrated_id);
    expect(api.updateHosts).toHaveBeenCalledOnce();
    expect(window.localStorage.getItem("rmux.remote_hosts")).toBeNull();
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
    expect(saved.schema_version).toBe(8);
    expect(saved.hosts).toBeUndefined();
    expect(saved.ssh_gateways).toBeUndefined();
    expect(api.updateHosts).not.toHaveBeenCalled();
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

  it("keeps a failed save visible through read-only SSH discovery until a write succeeds", async () => {
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    api.updateWorkspace.mockRejectedValueOnce(new Error("disk full"));
    act(() => result.current.setTabs([]));
    await waitFor(() => expect(result.current.error).toBe("disk full"));
    api.listSshConfigHosts.mockResolvedValue({ hosts: [{ destination: "new-alias" }], warnings: [] });
    await act(async () => { await result.current.refreshSshConfig(); });
    expect(result.current.hosts).toContainEqual(expect.objectContaining({
      name: "new-alias", source: "ssh_config",
    }));
    expect(result.current.error).toBe("disk full");
    expect(api.updateWorkspace).toHaveBeenCalledOnce();
    expect(api.updateHosts).not.toHaveBeenCalled();
    await act(async () => { await result.current.persist(true); });
    expect(result.current.error).toBeNull();
    expect(api.updateWorkspace).toHaveBeenCalledTimes(2);
  });

  it("publishes host settings only after saving and retains observations made during the write", async () => {
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    let finish!: (snapshot: HostCatalogSnapshot) => void;
    api.updateHosts.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const host = { ...result.current.hosts[1], name: "Build machine" };
    let saved!: Promise<void>;
    act(() => { saved = result.current.replaceView((current) => updateHostSettings(current, host)); });
    await waitFor(() => expect(api.updateHosts).toHaveBeenCalledOnce());
    expect(result.current.hosts[1].name).toBe("server");
    act(() => {
      result.current.update("sidebar_view", "ports");
      result.current.setSessions((sessions) => sessions.map((session) => ({ ...session, name: "observed shell", status: "running" })));
    });
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    await act(async () => {
      finish({ revision: "host-saved", document: api.updateHosts.mock.calls[0][1] });
      await saved;
      await result.current.persist();
    });
    expect(result.current.hosts[1].name).toBe("Build machine");
    expect(result.current.sidebar_view).toBe("ports");
    expect(result.current.sessions[0]).toMatchObject({ name: "observed shell", status: "running" });
    const document = api.updateWorkspace.mock.calls.slice(-1)[0][1];
    expect(api.updateHosts).toHaveBeenCalledOnce();
    expect(api.updateHosts.mock.calls[0][1].hosts[0].name).toBe("Build machine");
    expect(document.hosts).toBeUndefined();
    expect(document.sidebar_view).toBe("ports");
    expect(document.sessions[0].name).toBe("observed shell");
  });

  it("keeps failed host edits out of autosaves and retries the same staged change", async () => {
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.ready).toBe(true));
    let fail!: (reason: Error) => void;
    api.updateHosts.mockImplementationOnce(() => new Promise((_resolve, reject) => { fail = reject; }));
    const host = { ...result.current.hosts[1], name: "Build machine" };
    let saved!: Promise<void>;
    act(() => { saved = result.current.replaceView((current) => updateHostSettings(current, host)); });
    const rejected = saved.catch((error: Error) => error.message);
    await waitFor(() => expect(api.updateHosts).toHaveBeenCalledOnce());
    act(() => result.current.update("sidebar_view", "tasks"));
    await act(async () => {
      fail(new Error("disk full"));
      expect(await rejected).toBe("disk full");
      await result.current.persist();
    });
    expect(result.current.hosts[1].name).toBe("server");
    expect(result.current.sidebar_view).toBe("tasks");
    expect(api.updateHosts).toHaveBeenCalledOnce();
    expect(api.updateWorkspace.mock.calls.slice(-1)[0][1].hosts).toBeUndefined();
    await act(async () => result.current.replaceView((current) => updateHostSettings(current, host)));
    expect(result.current.hosts[1].name).toBe("Build machine");
    expect(result.current.hosts).toHaveLength(2);
    expect(api.updateHosts).toHaveBeenCalledTimes(2);
    expect(api.updateHosts.mock.calls[1][1].hosts).toHaveLength(1);
  });

  it("waits for pending writes before destroying the window and freezes later changes", async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() =>
      expect(nativeWindow.onCloseRequested).toHaveBeenCalledOnce(),
    );
    let complete!: (snapshot: WorkspaceSnapshot) => void;
    api.updateWorkspace.mockImplementation(
      () =>
        new Promise((resolve) => {
          complete = resolve;
        }),
    );
    act(() => result.current.setTabs([]));
    await waitFor(() => expect(api.updateWorkspace).toHaveBeenCalledOnce());
    const preventDefault = vi.fn();
    let closing!: Promise<void>;
    act(() => {
      closing = nativeWindow.onCloseRequested.mock.calls[0][0]({
        preventDefault,
      });
    });
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(result.current.closing).toBe(true);
    expect(nativeWindow.destroy).not.toHaveBeenCalled();
    act(() => result.current.setSessions([]));
    expect(result.current.sessions).toHaveLength(1);
    await act(async () => {
      complete({
        revision: "saved",
        document: api.updateWorkspace.mock.calls[0][1],
      });
      await closing;
    });
    expect(nativeWindow.destroy).toHaveBeenCalledOnce();
  });

  it("pauses window close for an in-flight session creation or a save failure", async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() =>
      expect(nativeWindow.onCloseRequested).toHaveBeenCalledOnce(),
    );
    result.current.closeBlockedRef.current = () => true;
    await act(async () =>
      nativeWindow.onCloseRequested.mock.calls[0][0]({
        preventDefault: vi.fn(),
      }),
    );
    expect(nativeWindow.destroy).not.toHaveBeenCalled();
    expect(result.current.error).toContain("ongoing operations");
    result.current.closeBlockedRef.current = () => false;
    api.updateWorkspace.mockRejectedValue({
      code: "workspace_io_failed",
      message: "disk full",
    });
    act(() => result.current.setTabs([]));
    await act(async () =>
      nativeWindow.onCloseRequested.mock.calls[0][0]({
        preventDefault: vi.fn(),
      }),
    );
    expect(result.current.closing).toBe(false);
    expect(result.current.error).toContain("disk full");
    expect(nativeWindow.destroy).not.toHaveBeenCalled();
  });

  it("does not discard malformed legacy settings", async () => {
    api.loadWorkspace.mockResolvedValue({
      revision: null,
      document: workspaceDocument(emptyWorkspaceView()),
    });
    api.loadHosts.mockResolvedValue(emptyHosts);
    const legacy = JSON.stringify({
      schema_version: 2,
      ssh_hosts: [{ destination: "invalid", port: 0 }],
    });
    window.localStorage.setItem("rmux.remote_hosts", legacy);
    const { result } = renderHook(() => useWorkspace());
    await waitFor(() => expect(result.current.error).toContain("preserved"));
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(window.localStorage.getItem("rmux.remote_hosts")).toBe(legacy);
  });
});
