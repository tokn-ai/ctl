// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import type {
  ConnectionTarget,
  SessionSummary,
  WorkspaceDocument,
  WorkspaceSnapshot,
} from "../lib/types";
import { restoreWorkspace } from "../features/workspace/workspaceModel";
import { TerminalPage } from "./TerminalPage";
import { detectShortcutPlatform } from "../features/commands/keybindings";

const api = vi.hoisted(() => ({
  loadWorkspace: vi.fn(),
  updateWorkspace: vi.fn(),
  listSessions: vi.fn(),
  inspectKnownSessions: vi.fn(),
  listSshConfigHosts: vi.fn(),
  setNativeWindowTitle: vi.fn(),
  createSession: vi.fn(),
  killSession: vi.fn(),
  restartLocalDaemon: vi.fn(),
  forgetSshCredentials: vi.fn(),
  probeSshHost: vi.fn(),
  cancelSshProbe: vi.fn(),
}));
const attachment = vi.hoisted(() => ({
  state: {
    phase: "idle",
    error_code: null,
    attachment_id: null,
    session: null,
    input_lease: { held: false, owned_by_client: false },
    layout_lease: { held: false, owned_by_client: false },
    shell_state: null,
    applied_sequence: null,
    reconnect_sequence: null,
    history_gap: false,
    terminal_size_mismatch: false,
    resize_with_window: false,
    message: null,
  },
  connect: vi.fn(),
  reconnect: vi.fn(),
  detach: vi.fn(),
  handleInput: vi.fn(),
  toggleInputLease: vi.fn(),
  toggleResizeWithWindow: vi.fn(),
  cancelPendingConnection: vi.fn(),
  resetAfterDaemonRestart: vi.fn(),
}));
vi.mock("../lib/tauri", async (original) => ({
  ...(await original<object>()),
  ...api,
}));
vi.mock("../features/attachment/useAttachment", () => ({
  useAttachment: () => attachment,
}));
vi.mock("../components/terminal/TerminalSurface", () => ({
  TerminalSurface: () => <div>Terminal renderer</div>,
}));
vi.mock("../features/commands/useNativeCommandEvents", () => ({
  useNativeCommandEvents: () => {},
}));

function snapshot(): WorkspaceSnapshot {
  return {
    revision: "one",
    document: {
      schema_version: 1,
      workspace_id: "default",
      hosts: [
        { host_id: "local", target: { kind: "local" } },
        { host_id: "test-id", target: { kind: "ssh", destination: "test" } },
        {
          host_id: "unused-id",
          target: { kind: "ssh", destination: "unused" },
        },
      ],
      sessions: [
        {
          host_id: "test-id",
          session_id: "known-id",
          name: "remembered",
          last_known_cwd: "/work",
          last_known_cwd_display: "~/work",
        },
      ],
      tabs: [{ host_id: "test-id", session_id: "known-id" }],
      active_tab: { host_id: "test-id", session_id: "known-id" },
    },
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  window.localStorage.clear();
  api.loadWorkspace.mockResolvedValue(snapshot());
  api.updateWorkspace.mockImplementation(
    async (_revision: string | null, document: WorkspaceDocument) => ({
      revision: crypto.randomUUID(),
      document,
    }),
  );
  api.listSshConfigHosts.mockResolvedValue({
    hosts: [{ destination: "only-in-ssh-config" }],
    warnings: [],
  });
  api.setNativeWindowTitle.mockResolvedValue(undefined);
  api.forgetSshCredentials.mockResolvedValue(undefined);
  api.probeSshHost.mockResolvedValue(undefined);
  api.cancelSshProbe.mockResolvedValue(undefined);
  api.inspectKnownSessions.mockResolvedValue([]);
  attachment.connect.mockResolvedValue(undefined);
  attachment.detach.mockResolvedValue(undefined);
});
afterEach(cleanup);

function newSession(
  target: ConnectionTarget = { kind: "local" },
): SessionSummary {
  return {
    target,
    session_id: "created-id",
    name: "created-shell",
    status: "running",
    next_sequence: "0",
    terminal_size: {
      columns: 80,
      rows: 24,
      pixel_width: null,
      pixel_height: null,
    },
  };
}

function shortcut(code: string, shiftKey = true) {
  const macos = detectShortcutPlatform() === "macos";
  fireEvent.keyDown(window, {
    code,
    ctrlKey: !macos,
    metaKey: macos,
    shiftKey,
  });
}

describe("workspace-backed terminal page", () => {
  it("opens new-shell quick input from its shortcut and blocks other commands until cancelled", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyN");
    const dialog = screen.getByRole("dialog", {
      name: "New shell — host · 1/2",
    });
    expect(document.querySelector("main")?.hasAttribute("inert")).toBe(true);
    shortcut("KeyP");
    shortcut("KeyN");
    shortcut("KeyE", detectShortcutPlatform() !== "macos");
    expect(screen.getAllByRole("dialog")).toEqual([dialog]);
    expect(api.createSession).not.toHaveBeenCalled();
    expect(api.killSession).not.toHaveBeenCalled();
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.querySelector("main")?.hasAttribute("inert")).toBe(false);
    shortcut("KeyN");
    expect(document.activeElement).toBe(
      screen.getByRole("option", { name: "Local" }),
    );
  });

  it("routes palette New Shell to a chosen remote without contacting other hosts", async () => {
    const target = {
      kind: "ssh" as const,
      destination: "test",
      host_id: "test-id",
    };
    const created = newSession(target);
    api.createSession.mockResolvedValue(created);
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyP");
    fireEvent.change(
      screen.getByRole("combobox", { name: "Search commands" }),
      {
        target: { value: "New Shell" },
      },
    );
    fireEvent.click(screen.getByRole("option", { name: /New Shell/ }));
    expect(
      screen.queryByRole("dialog", { name: "Command palette" }),
    ).toBeNull();
    fireEvent.click(screen.getByRole("option", { name: "test" }));
    fireEvent.change(screen.getByLabelText("Working directory"), {
      target: { value: "/remote/work" },
    });
    expect(api.createSession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.createSession).toHaveBeenCalledExactlyOnceWith({
      target,
      working_directory: "/remote/work",
      terminal_size: {
        columns: 80,
        rows: 24,
        pixel_width: null,
        pixel_height: null,
      },
    });
    expect(attachment.connect).toHaveBeenCalledExactlyOnceWith(created, {
      resize_with_window: true,
    });
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.probeSshHost).not.toHaveBeenCalled();
  });

  it("keeps backend failures in the dialog for correction and retry", async () => {
    api.createSession
      .mockRejectedValueOnce({ message: "Cannot open directory" })
      .mockResolvedValue(newSession());
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    fireEvent.click(screen.getByRole("option", { name: "Local" }));
    fireEvent.change(screen.getByLabelText("Working directory"), {
      target: { value: "/missing" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Cannot open directory",
    );
    expect(attachment.connect).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("Working directory"), {
      target: { value: "/work" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.createSession).toHaveBeenCalledTimes(2);
    expect(attachment.connect).toHaveBeenCalledOnce();
  });

  it("does not offer another creation when opening the already-created shell fails", async () => {
    const created = newSession();
    api.createSession.mockResolvedValue(created);
    attachment.connect.mockRejectedValueOnce(
      new Error("Connection interrupted"),
    );
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    fireEvent.click(screen.getByRole("option", { name: "Local" }));
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await screen.findByText(/was created, but opening its tab failed/);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.createSession).toHaveBeenCalledOnce();
    fireEvent.click(
      screen.getByRole("button", { name: "Shell — created-shell" }),
    );
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledTimes(2));
    expect(api.createSession).toHaveBeenCalledOnce();
    expect(api.killSession).not.toHaveBeenCalled();
  });

  it("keeps a restored remote tab cold, then resumes it after connecting its host", async () => {
    const known = restoreWorkspace(snapshot().document).sessions[0];
    api.inspectKnownSessions.mockResolvedValueOnce([
      {
        session_id: known.session_id,
        session: { ...known, status: "running", next_sequence: "42" },
        shell_state: null,
        error: null,
      },
    ]);
    let authenticated!: () => void;
    api.probeSshHost.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          authenticated = resolve;
        }),
    );
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    expect(screen.getByText("unverified", { exact: false })).toBeTruthy();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Connect host" }));
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    await act(async () => authenticated());
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledTimes(1));
    expect(attachment.connect.mock.calls[0][0]).toMatchObject({
      session_id: "known-id",
      target: { host_id: "test-id", destination: "test" },
      status: "running",
      next_sequence: "42",
    });
    expect(api.inspectKnownSessions).toHaveBeenCalledExactlyOnceWith(
      { kind: "ssh", destination: "test", host_id: "test-id" },
      ["known-id"],
    );
    expect(api.listSessions).not.toHaveBeenCalled();
  });

  it("automatically attaches only the selected local tab once across rerenders", async () => {
    const saved = snapshot();
    saved.document.sessions.push({
      host_id: "local",
      session_id: "local-id",
      name: "local-shell",
      last_known_cwd: "/local",
      last_known_cwd_display: "~/local",
    });
    saved.document.active_tab = { host_id: "local", session_id: "local-id" };
    saved.document.tabs.push(saved.document.active_tab);
    api.loadWorkspace.mockResolvedValue(saved);
    const page = render(
      <StrictMode>
        <TerminalPage />
      </StrictMode>,
    );
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledOnce());
    expect(attachment.connect.mock.calls[0]).toEqual([
      expect.objectContaining({
        session_id: "local-id",
        target: { kind: "local" },
      }),
      { resize_with_window: false },
    ]);
    page.rerender(
      <StrictMode>
        <TerminalPage />
      </StrictMode>,
    );
    expect(attachment.connect).toHaveBeenCalledOnce();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
  });

  it("does not attach after failed or cancelled host authentication", async () => {
    api.probeSshHost.mockRejectedValueOnce(new Error("Authentication failed"));
    render(<TerminalPage />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Connect host" }),
    );
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await screen.findByText("Authentication failed");
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();

    let authenticated!: () => void;
    api.probeSshHost.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          authenticated = resolve;
        }),
    );
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel quick input" }));
    await act(async () => authenticated());
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(api.cancelSshProbe).toHaveBeenCalledOnce();
  });

  it("refreshes only remembered IDs and keeps missing sessions in the workspace", async () => {
    api.inspectKnownSessions.mockResolvedValue([
      {
        session_id: "known-id",
        session: null,
        shell_state: null,
        error: { code: "session_not_found", message: "gone" },
      },
    ]);
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh sessions" }));
    await waitFor(() =>
      expect(api.inspectKnownSessions).toHaveBeenCalledTimes(1),
    );
    expect(api.inspectKnownSessions.mock.calls[0]).toEqual([
      { kind: "ssh", destination: "test", host_id: "test-id" },
      ["known-id"],
    ]);
    await screen.findByText("missing", { exact: false });
    expect(
      screen.getByRole("button", { name: "~/work — remembered" }),
    ).toBeTruthy();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("keeps unreachable sessions distinct from confirmed missing ones", async () => {
    api.inspectKnownSessions.mockRejectedValue({
      code: "ssh_authentication_failed",
      message: "Authenticate first",
    });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh sessions" }));
    await screen.findByText("unreachable", { exact: false });
    expect(screen.getByText("Authenticate first")).toBeTruthy();
    expect(screen.queryByText("missing", { exact: false })).toBeNull();
    expect(api.killSession).not.toHaveBeenCalled();
  });

  it("removes workspace membership without terminating the daemon session", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(
      screen.getByRole("button", { name: "Remove remembered from workspace" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Remove from workspace" }),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "~/work — remembered" }),
      ).toBeNull(),
    );
    expect(api.killSession).not.toHaveBeenCalled();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    await waitFor(() => {
      const latest =
        api.updateWorkspace.mock.calls[
          api.updateWorkspace.mock.calls.length - 1
        ][1];
      expect(latest.sessions).toEqual([]);
      expect(latest.tabs).toEqual([]);
    });
  });

  it("keeps a detached tab's session known and only explicit termination calls kill", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(
      screen.getByRole("button", { name: "Disconnect from remembered" }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Connect host" })).toBeNull(),
    );
    expect(
      screen.getByRole("button", { name: "~/work — remembered" }),
    ).toBeTruthy();
    expect(api.killSession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Close remembered" }));
    fireEvent.click(screen.getByRole("button", { name: "Close session" }));
    await waitFor(() =>
      expect(api.killSession).toHaveBeenCalledExactlyOnceWith({
        target: { kind: "ssh", destination: "test", host_id: "test-id" },
        session_id: "known-id",
      }),
    );
  });

  it("saves a newly created local session before attachment, then reconnects it on relaunch", async () => {
    const created = {
      target: { kind: "local" as const },
      session_id: "new-local",
      name: "new-shell",
      status: "running" as const,
      next_sequence: "0",
      terminal_size: {
        columns: 80,
        rows: 24,
        pixel_width: null,
        pixel_height: null,
      },
    };
    api.createSession.mockResolvedValue(created);
    let disk = snapshot();
    api.updateWorkspace.mockImplementation(
      async (_revision: string | null, document: WorkspaceDocument) => {
        disk = { revision: crypto.randomUUID(), document };
        return disk;
      },
    );
    let release!: () => void;
    api.updateWorkspace.mockImplementationOnce(
      (_revision: string | null, document: WorkspaceDocument) =>
        new Promise((resolve) => {
          release = () => {
            disk = { revision: "created", document };
            resolve(disk);
          };
        }),
    );
    const first = render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    fireEvent.click(screen.getByRole("option", { name: "Local" }));
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(api.updateWorkspace).toHaveBeenCalledOnce());
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(api.updateWorkspace.mock.calls[0][1].sessions).toHaveLength(2);
    await act(async () => release());
    await waitFor(() =>
      expect(attachment.connect).toHaveBeenCalledExactlyOnceWith(created, {
        resize_with_window: true,
      }),
    );
    await waitFor(() =>
      expect(disk.document.active_tab).toEqual({
        host_id: "local",
        session_id: "new-local",
      }),
    );
    first.unmount();
    attachment.connect.mockClear();
    api.loadWorkspace.mockResolvedValue(disk);
    render(<TerminalPage />);
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledOnce());
    expect(
      screen.getByRole("button", { name: "Shell — new-shell" }),
    ).toBeTruthy();
    expect(attachment.connect.mock.calls[0][0]).toMatchObject({
      session_id: "new-local",
      target: { kind: "local" },
    });
    expect(api.listSessions).not.toHaveBeenCalled();
  });

  it("does not attach, kill, or duplicate a shell when saving after creation fails", async () => {
    api.createSession.mockResolvedValue({
      target: { kind: "local" },
      session_id: "unsaved",
      name: "created-once",
      status: "running",
      next_sequence: "0",
      terminal_size: {
        columns: 80,
        rows: 24,
        pixel_width: null,
        pixel_height: null,
      },
    });
    api.updateWorkspace.mockRejectedValue({
      code: "workspace_io_failed",
      message: "disk full",
    });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    fireEvent.click(screen.getByRole("option", { name: "Local" }));
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await screen.findByText(
      /was created, but saving its workspace entry failed/,
    );
    expect(api.createSession).toHaveBeenCalledOnce();
    expect(api.killSession).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Retry saving" })).toBeTruthy();
  });
});
