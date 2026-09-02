// @vitest-environment jsdom
import {
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
});
