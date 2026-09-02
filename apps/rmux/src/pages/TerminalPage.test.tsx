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
import type { WorkspaceDocument, WorkspaceSnapshot } from "../lib/types";
import { TerminalPage } from "./TerminalPage";

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
  api.inspectKnownSessions.mockResolvedValue([]);
  attachment.connect.mockResolvedValue(undefined);
  attachment.detach.mockResolvedValue(undefined);
});
afterEach(cleanup);

describe("workspace-backed terminal page", () => {
  it("restores its selected tab without listing or connecting any host", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect session" });
    expect(screen.getByText("unverified", { exact: false })).toBeTruthy();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Connect session" }));
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledTimes(1));
    expect(attachment.connect.mock.calls[0][0]).toMatchObject({
      session_id: "known-id",
      target: { host_id: "test-id", destination: "test" },
    });
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
    await screen.findByRole("button", { name: "Connect session" });
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
    await screen.findByRole("button", { name: "Connect session" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh sessions" }));
    await screen.findByText("unreachable", { exact: false });
    expect(screen.getByText("Authenticate first")).toBeTruthy();
    expect(screen.queryByText("missing", { exact: false })).toBeNull();
    expect(api.killSession).not.toHaveBeenCalled();
  });

  it("removes workspace membership without terminating the daemon session", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect session" });
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
    await screen.findByRole("button", { name: "Connect session" });
    fireEvent.click(
      screen.getByRole("button", { name: "Disconnect from remembered" }),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Connect session" }),
      ).toBeNull(),
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

  it("saves a newly created session before attachment, then restores it disconnected on relaunch", async () => {
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
    await screen.findByRole("button", { name: "Connect session" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
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
    await screen.findByRole("button", { name: "Connect session" });
    expect(
      screen.getByRole("button", { name: "Shell — new-shell" }),
    ).toBeTruthy();
    expect(attachment.connect).not.toHaveBeenCalled();
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
    await screen.findByRole("button", { name: "Connect session" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
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
