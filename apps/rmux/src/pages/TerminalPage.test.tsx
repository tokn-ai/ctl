// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import type {
  AttachmentViewState,
  ConnectionTarget,
  SessionSummary,
  WorkspaceDocument,
  WorkspaceSnapshot,
  HostCatalogDocument,
  HostCatalogSnapshot,
  KeybindingsDocument,
  TaskDefinition,
  TaskDefinitionScope,
  SavedTaskDefinition,
  WorkspacePortForward,
  SshPrompt,
} from "../lib/types";
import { projectedHostId, restoreWorkspace } from "../features/workspace/workspaceModel";
import { TerminalPage } from "./TerminalPage";
import { detectShortcutPlatform } from "../features/commands/keybindings";
import { COMMAND_IDS } from "../features/commands/terminalCommands";
import { NATIVE_COMMAND_EVENT } from "../features/commands/useNativeCommandEvents";

const nativeEvents = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: string }) => void>(),
}));
const nativeWindow = vi.hoisted(() => ({
  onCloseRequested: vi.fn(),
  destroy: vi.fn(),
}));
const remoteInfo = { remote_id: "ad6a8b53-bae0-45ce-8f09-5cb084a6c843", agent_version: "0.1.0" };

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    async (name: string, callback: (event: { payload: string }) => void) => {
      nativeEvents.listeners.set(name, callback);
      return () => nativeEvents.listeners.delete(name);
    },
  ),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => nativeWindow,
}));

const api = vi.hoisted(() => ({
  taskRequest: vi.fn(),
  loadTaskDefinitions: vi.fn(),
  saveTaskDefinition: vi.fn(),
  removeTaskDefinition: vi.fn(),
  restartTaskDaemon: vi.fn(),
  loadWorkspace: vi.fn(),
  updateWorkspace: vi.fn(),
  loadHosts: vi.fn(),
  updateHosts: vi.fn(),
  listSessions: vi.fn(),
  inspectKnownSessions: vi.fn(),
  listSshConfigHosts: vi.fn(),
  setNativeWindowTitle: vi.fn(),
  createSession: vi.fn(),
  killSession: vi.fn(),
  restartLocalDaemon: vi.fn(),
  forgetSshCredentials: vi.fn(),
  probeSshHost: vi.fn(),
  sshConnectionStatus: vi.fn(),
  disconnectSshHost: vi.fn(),
  cancelSshProbe: vi.fn(),
  respondSshPrompt: vi.fn(),
  configurePortForward: vi.fn(),
  listPortForwards: vi.fn(),
  listRemoteListeners: vi.fn(),
  checkLocalPort: vi.fn(),
  loadKeybindings: vi.fn(),
  saveKeybindings: vi.fn(),
  syncCommandMenu: vi.fn(),
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

function snapshot(): WorkspaceSnapshot {
  return {
    revision: "one",
    document: {
      schema_version: 8,
      workspace_id: "default",
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

function hostSnapshot(): HostCatalogSnapshot {
  return {
    revision: "hosts-one",
    document: {
      schema_version: 1,
      hosts: ["test", "unused"].map((destination) => ({
        host_id: `${destination}-id`,
        name: destination,
        preferred_method_id: "default",
        connection_methods: [{ method_id: "default", name: "SSH", target: { kind: "ssh", destination } }],
      })),
      ssh_gateways: [],
    },
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  attachment.state.phase = "idle";
  attachment.state.session = null;
  attachment.state.shell_state = null;
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  nativeWindow.onCloseRequested.mockResolvedValue(() => {});
  nativeWindow.destroy.mockResolvedValue(undefined);
  api.restartTaskDaemon.mockResolvedValue(undefined);
  api.taskRequest.mockResolvedValue({ type: "task_list", tasks: [] });
  api.loadTaskDefinitions.mockImplementation(async (scope: TaskDefinitionScope) => ({ scope, path: "/test/definitions.json", definitions: [] }));
  api.saveTaskDefinition.mockImplementation(async (_scope: TaskDefinitionScope, definition_id: string, _expected_revision: string | null, definition: TaskDefinition) => ({ definition_id, revision: "saved-revision", definition }));
  api.removeTaskDefinition.mockResolvedValue(undefined);
  nativeEvents.listeners.clear();
  window.localStorage.clear();
  api.loadWorkspace.mockResolvedValue(snapshot());
  api.loadHosts.mockResolvedValue(hostSnapshot());
  api.loadKeybindings.mockResolvedValue({
    path: "/test/keybindings.json",
    revision: null,
    document: { schema_version: 1, overrides: [] },
  });
  api.syncCommandMenu.mockResolvedValue(undefined);
  api.saveKeybindings.mockImplementation(
    async (_revision: string | null, document: KeybindingsDocument) => ({
      path: "/test/keybindings.json",
      revision: JSON.stringify(document),
      document,
    }),
  );
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
  api.listSshConfigHosts.mockResolvedValue({
    hosts: [{ destination: "only-in-ssh-config" }],
    warnings: [],
  });
  api.setNativeWindowTitle.mockResolvedValue(undefined);
  api.forgetSshCredentials.mockResolvedValue(undefined);
  api.probeSshHost.mockReset().mockResolvedValue(remoteInfo);
  api.sshConnectionStatus.mockReset().mockResolvedValue({ connected: true, manually_disconnected: false });
  api.disconnectSshHost.mockReset().mockResolvedValue(undefined);
  api.cancelSshProbe.mockResolvedValue(undefined);
  api.respondSshPrompt.mockReset().mockResolvedValue(undefined);
  api.listPortForwards.mockResolvedValue([]);
  api.listRemoteListeners.mockResolvedValue({ listeners: [], warnings: [] });
  api.checkLocalPort.mockImplementation(async (port: number) => ({ port, available: true, message: null }));
  api.configurePortForward.mockImplementation(async (_target: ConnectionTarget, forward: WorkspacePortForward) => ({
    forward, state: "active", message: null,
  }));
  api.inspectKnownSessions.mockResolvedValue([]);
  api.killSession.mockResolvedValue(undefined);
  attachment.connect.mockResolvedValue(undefined);
  attachment.detach.mockResolvedValue(undefined);
});
afterEach(() => {
  cleanup();
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  vi.restoreAllMocks();
});

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

function nativeCommand(commandId: string, count = 1) {
  const listener = nativeEvents.listeners.get(NATIVE_COMMAND_EVENT);
  expect(listener).toBeDefined();
  act(() => {
    for (let index = 0; index < count; index += 1) {
      listener!({ payload: commandId });
    }
  });
}

describe("workspace-backed terminal page", () => {
  it("switches sidebar tabs and keeps an incomplete draft after dismissal and relaunch", async () => {
    const first = render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    expect(screen.queryByRole("button", { name: "New shell" })).toBeNull();
    const existingTabs = within(
      screen.getByRole("navigation", { name: "Workspace tabs" }),
    ).getAllByRole("tab").length;
    fireEvent.click(
      screen.getByRole("button", { name: "New task definition" }),
    );
    const editor = screen.getByRole("dialog", { name: "Create task" });
    fireEvent.change(within(editor).getByRole("textbox", { name: "Name" }), {
      target: { value: "Half written" },
    });
    fireEvent.change(
      within(editor).getByRole("textbox", { name: "Working directory" }),
      { target: { value: "unfinished/path" } },
    );
    fireEvent.keyDown(editor, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(
      within(
        screen.getByRole("navigation", { name: "Workspace tabs" }),
      ).getAllByRole("tab"),
    ).toHaveLength(existingTabs);
    await waitFor(() => {
      const document = api.updateWorkspace.mock.calls.slice(
        -1,
      )[0]?.[1] as WorkspaceDocument;
      expect(document.task_drafts?.[0].definition).toMatchObject({
        name: "Half written",
        program: "",
        working_directory: "unfinished/path",
      });
    });
    const persisted = api.updateWorkspace.mock.calls.slice(
      -1,
    )[0]![1] as WorkspaceDocument;
    expect(persisted.task_definitions).toBeUndefined();
    expect(
      api.taskRequest.mock.calls.every(
        ([request]) => request.type === "list_tasks",
      ),
    ).toBe(true);
    first.unmount();
    api.loadWorkspace.mockResolvedValue({
      revision: "saved-draft",
      document: persisted,
    });
    render(<TerminalPage />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Resume draft Half written" }),
    );
    expect(
      (
        screen.getByRole("textbox", {
          name: "Name",
        }) as HTMLInputElement
      ).value,
    ).toBe("Half written");
    expect(
      (screen.getByRole("textbox", { name: "Executable" }) as HTMLInputElement)
        .value,
    ).toBe("");
    fireEvent.click(screen.getByRole("button", { name: "Close task editor" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(screen.getByRole("tab", { name: "Sessions" }));
    expect(screen.getByRole("button", { name: "New shell" })).toBeDefined();
  });

  it("creates a definition explicitly from the autosaved draft without starting a task", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    fireEvent.click(
      screen.getByRole("button", { name: "New task definition" }),
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), {
      target: { value: "Build" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Executable" }), {
      target: { value: "cargo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create definition" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const document = api.updateWorkspace.mock.calls.slice(
      -1,
    )[0][1] as WorkspaceDocument;
    expect(document.task_definitions).toBeUndefined();
    expect(api.saveTaskDefinition.mock.calls[0][3]).toMatchObject({
      name: "Build",
      program: "cargo",
    });
    expect(document.task_drafts).toEqual([]);
    expect(document.task_references).toEqual([]);
    expect(
      api.taskRequest.mock.calls.every(
        ([request]) => request.type === "list_tasks",
      ),
    ).toBe(true);
  });

  it("generates a name when creating a definition with a blank name", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    fireEvent.click(screen.getByRole("button", { name: "New task definition" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Command line" }), {
      target: { value: "cargo build" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create definition" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const document = api.updateWorkspace.mock.calls.slice(-1)[0][1] as WorkspaceDocument;
    expect(api.saveTaskDefinition.mock.calls[0][3].name).toMatch(/^cargo-default-[a-z]+$/);
    expect(document.task_drafts).toEqual([]);
  });

  it("parses command input and preserves unfinished quoting on dismissal", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    fireEvent.click(
      screen.getByRole("button", { name: "New task definition" }),
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Command line" }), {
      target: { value: 'cargo run --bin "api server"' },
    });
    expect(
      (screen.getByRole("textbox", { name: "Executable" }) as HTMLInputElement)
        .value,
    ).toBe("cargo");
    expect(
      (screen.getByRole("textbox", { name: "Argument 3" }) as HTMLInputElement)
        .value,
    ).toBe("api server");
    fireEvent.change(screen.getByRole("textbox", { name: "Command line" }), {
      target: { value: 'cargo "unfinished' },
    });
    fireEvent.click(screen.getByRole("button", { name: "Close task editor" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Resume draft Untitled task" }),
    );
    expect(
      (
        screen.getByRole("textbox", {
          name: "Command line",
        }) as HTMLInputElement
      ).value,
    ).toBe('cargo "unfinished');
    fireEvent.click(screen.getByRole("button", { name: "Create definition" }));
    expect(screen.getByRole("dialog", { name: "Create task" })).toBeDefined();
    await waitFor(() => {
      const document = api.updateWorkspace.mock.calls.slice(
        -1,
      )[0][1] as WorkspaceDocument;
      expect(document.task_drafts?.[0].command_line).toBe('cargo "unfinished');
      expect(document.task_definitions).toBeUndefined();
    });
  });

  it("keeps a dirty draft's revision across external refresh and requires explicit reload after a conflict", async () => {
    const scope = { kind: "global" as const };
    const original = { definition_id: "shared-build", revision: "r1", definition: { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" as const } };
    api.loadTaskDefinitions.mockResolvedValue({ scope, path: "/shared/tasks.json", definitions: [original] });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    fireEvent.click(await screen.findByText("Build"));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "My draft" } });

    const external = { ...original, revision: "r2", definition: { ...original.definition, name: "CLI edit" } };
    api.loadTaskDefinitions.mockResolvedValue({ scope, path: "/shared/tasks.json", definitions: [external] });
    fireEvent(window, new Event("focus"));
    await screen.findByText("CLI edit");
    expect((screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement).value).toBe("My draft");
    api.saveTaskDefinition.mockRejectedValueOnce({ code: "definition_conflict", message: "Definition revision conflict." });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await screen.findAllByText("Definition revision conflict.");
    expect(api.saveTaskDefinition).toHaveBeenLastCalledWith(scope, "shared-build", "r1", expect.objectContaining({ name: "My draft" }));
    const draftDocument = api.updateWorkspace.mock.calls.slice(-1)[0][1] as WorkspaceDocument;
    expect(draftDocument.task_drafts?.[0]).toMatchObject({ scope, base_revision: "r1", definition: { name: "My draft" } });
    expect(draftDocument.task_definitions).toBeUndefined();

    fireEvent.click(screen.getByRole("button", { name: "Reload latest definition…" }));
    fireEvent.click(screen.getByRole("button", { name: "Replace draft with latest" }));
    await waitFor(() => expect((screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement).value).toBe("CLI edit"));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "Reviewed edit" } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.saveTaskDefinition).toHaveBeenLastCalledWith(scope, "shared-build", "r2", expect.objectContaining({ name: "Reviewed edit" }));
  });

  it.each(["Build", "Test"])("keeps a reopened %s editor open when an earlier save completes", async (next) => {
    const scope = { kind: "global" as const };
    const original: SavedTaskDefinition = { definition_id: "build", revision: "r1", definition: { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" } };
    const other = { ...original, definition_id: "test", definition: { ...original.definition, name: "Test" } };
    api.loadTaskDefinitions.mockResolvedValue({ scope, path: "/shared/tasks.json", definitions: [original, other] });
    let finish!: (value: SavedTaskDefinition) => void;
    api.saveTaskDefinition.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    fireEvent.click(await screen.findByText("Build"));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "Saved build" } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(api.saveTaskDefinition).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Close task editor" }));
    fireEvent.click(screen.getByText(next));
    await act(async () => { finish({ ...original, revision: "r2", definition: { ...original.definition, name: "Saved build" } }); });
    await waitFor(() => expect((screen.getByRole("button", { name: "Save changes" }) as HTMLButtonElement).disabled).toBe(false));
    expect(screen.getByRole("dialog", { name: "Edit task definition" })).toBeDefined();
    expect((screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement).value).toBe(next === "Build" ? "Saved build" : "Test");
  });

  it("requires an absolute project folder and keeps a resumed draft pinned to that source", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Definition source" }), { target: { value: "project" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Project folder" }), { target: { value: "relative/project" } });
    fireEvent.click(screen.getByRole("button", { name: "Open project definitions" }));
    expect(screen.getByText("Enter an absolute project folder path.")).toBeDefined();
    expect(api.loadTaskDefinitions.mock.calls.every(([scope]) => scope.kind === "global")).toBe(true);
    fireEvent.change(screen.getByRole("textbox", { name: "Project folder" }), { target: { value: "/work/project" } });
    fireEvent.click(screen.getByRole("button", { name: "Open project definitions" }));
    const project = { kind: "project", project_root: "/work/project" };
    await waitFor(() => expect(api.loadTaskDefinitions).toHaveBeenCalledWith(project));
    fireEvent.click(screen.getByRole("button", { name: "New task definition" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "Project build" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Command line" }), { target: { value: "cargo build" } });
    fireEvent.click(screen.getByRole("button", { name: "Close task editor" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Definition source" }), { target: { value: "global" } });
    fireEvent.click(screen.getByRole("button", { name: "Resume draft Project build" }));
    fireEvent.click(screen.getByRole("button", { name: "Create definition" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.saveTaskDefinition).toHaveBeenLastCalledWith(project, expect.any(String), null, expect.objectContaining({ name: "Project build" }));
    const document = api.updateWorkspace.mock.calls.slice(-1)[0][1] as WorkspaceDocument;
    expect(document.task_definition_scope).toEqual({ kind: "global" });
    expect(document.task_definitions).toBeUndefined();
  });

  it("deletes through the shared store using the opened revision and retains the draft on conflict", async () => {
    const scope = { kind: "global" as const };
    const saved = { definition_id: "shared-build", revision: "r1", definition: { name: "Build", program: "cargo", arguments: [], working_directory: null, execution_mode: "background" as const } };
    api.loadTaskDefinitions.mockResolvedValue({ scope, path: "/shared/tasks.json", definitions: [saved] });
    api.removeTaskDefinition.mockRejectedValue({ code: "definition_conflict", message: "The definition changed before deletion." });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("tab", { name: "Tasks" }));
    fireEvent.click(await screen.findByText("Build"));
    fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "Unfinished edit" } });
    fireEvent.click(screen.getByRole("button", { name: "Delete definition…" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete definition" }));
    await screen.findAllByText("The definition changed before deletion.");
    expect(api.removeTaskDefinition).toHaveBeenCalledWith(scope, saved.definition_id, "r1");
    expect((screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement).value).toBe("Unfinished edit");
    expect((api.updateWorkspace.mock.calls.slice(-1)[0][1] as WorkspaceDocument).task_drafts?.[0].base_revision).toBe("r1");
  });

  it("restarts taskd from the palette and prevents duplicate requests while pending", async () => {
    let finish!: () => void;
    api.restartTaskDaemon.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyP");
    fireEvent.change(
      screen.getByRole("combobox", { name: "Search commands" }),
      {
        target: { value: "Restart taskd" },
      },
    );
    fireEvent.click(screen.getByRole("option", { name: /Restart taskd/ }));
    await screen.findByText("Restarting taskd…");
    nativeCommand(COMMAND_IDS.restartTaskDaemon, 2);
    expect(api.restartTaskDaemon).toHaveBeenCalledTimes(1);
    await act(async () => {
      finish();
    });
    await screen.findByText("taskd restarted.");
    expect(api.taskRequest).toHaveBeenCalledWith({ type: "list_tasks" });
    expect(api.restartLocalDaemon).not.toHaveBeenCalled();
  });

  it("shows taskd restart errors in the task sidebar", async () => {
    api.restartTaskDaemon.mockRejectedValueOnce(
      new Error("Stop active tasks before restarting taskd."),
    );
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    nativeCommand(COMMAND_IDS.restartTaskDaemon);
    await screen.findByText("Stop active tasks before restarting taskd.");
    expect(screen.queryByText("taskd restarted.")).toBeNull();
    expect(screen.queryByText("Restarting taskd…")).toBeNull();
  });

  it("saves a remapped shortcut through quick input, updates labels, and restores it on restart", async () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("Linux");
    const first = render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyP");
    fireEvent.change(
      screen.getByRole("combobox", { name: "Search commands" }),
      { target: { value: "Configure Keyboard Shortcuts" } },
    );
    fireEvent.click(
      screen.getByRole("option", { name: /Configure Keyboard Shortcuts/ }),
    );
    fireEvent.click(screen.getByRole("option", { name: /^New Shell/ }));
    fireEvent.change(screen.getByRole("textbox", { name: "Shortcut" }), {
      target: { value: "Primary+Shift+Y" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save shortcut" }));
    const picker = await screen.findByRole("dialog", {
      name: "Keyboard shortcuts",
    });
    expect(
      screen.getByRole("option", { name: /New Shell/ }).textContent,
    ).toContain("Ctrl+Shift+Y");
    expect(api.saveKeybindings).toHaveBeenCalledExactlyOnceWith(null, {
      schema_version: 1,
      overrides: [
        {
          command_id: COMMAND_IDS.newShell,
          keybinding: { code: "KeyY", primary: true, shift: true, alt: false },
        },
      ],
    });
    fireEvent.keyDown(picker, { key: "Escape" });
    shortcut("KeyN");
    expect(screen.queryByRole("dialog")).toBeNull();
    shortcut("KeyY");
    expect(
      screen.getByRole("dialog", { name: "New shell — host · 1/2" }),
    ).toBeTruthy();
    first.unmount();
    const document = api.saveKeybindings.mock.calls[0][1];
    api.loadKeybindings.mockResolvedValue({
      path: "/test/keybindings.json",
      revision: JSON.stringify(document),
      document,
    });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyN");
    expect(screen.queryByRole("dialog")).toBeNull();
    shortcut("KeyY");
    expect(
      screen.getByRole("dialog", { name: "New shell — host · 1/2" }),
    ).toBeTruthy();
  });

  it("uses the remapped close shortcut for both requesting and confirming, and supports unbinding", async () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("Linux");
    api.loadKeybindings.mockResolvedValue({
      path: "/test/keybindings.json",
      revision: "one",
      document: {
        schema_version: 1,
        overrides: [
          {
            command_id: COMMAND_IDS.close,
            keybinding: { code: "KeyY", primary: true, shift: true },
          },
          { command_id: COMMAND_IDS.newShell, keybinding: null },
        ],
      },
    });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyN");
    shortcut("KeyE");
    expect(screen.queryByRole("dialog")).toBeNull();
    shortcut("KeyY");
    expect(screen.getByRole("dialog").textContent).toContain(
      "Press Ctrl+Shift+Y to confirm",
    );
    expect(api.killSession).not.toHaveBeenCalled();
    shortcut("KeyY");
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Terminate remembered" }),
      ).toBeNull(),
    );
    expect(api.killSession).toHaveBeenCalledOnce();
  });

  it("dispatches configured dialog accept/cancel keys without exposing app commands", async () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("Linux");
    api.loadKeybindings.mockResolvedValue({
      path: "/test/keybindings.json",
      revision: "one",
      document: {
        schema_version: 1,
        overrides: [
          {
            command_id: "quick_input.cancel",
            keybinding: { code: "Escape", primary: false, alt: true },
          },
          {
            command_id: "quick_input.accept",
            keybinding: { code: "Enter", primary: true },
          },
        ],
      },
    });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyN");
    const picker = screen.getByRole("dialog");
    fireEvent.keyDown(picker, { key: "Escape" });
    expect(screen.getByRole("dialog")).toBe(picker);
    fireEvent.keyDown(picker, { code: "Enter", ctrlKey: true });
    const input = screen.getByRole("textbox", { name: "Working directory" });
    fireEvent.change(input, { target: { value: "/chosen/path" } });
    nativeCommand(COMMAND_IDS.close);
    expect(api.killSession).not.toHaveBeenCalled();
    api.createSession.mockResolvedValue(newSession());
    fireEvent.keyDown(input, { code: "Enter", ctrlKey: true });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.createSession.mock.calls[0][0].working_directory).toBe(
      "/chosen/path",
    );
    shortcut("KeyN");
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Escape",
      altKey: true,
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("preserves bindings and the editor draft when saving fails", async () => {
    api.saveKeybindings.mockRejectedValue(new Error("changed on disk"));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    nativeCommand(COMMAND_IDS.configureKeybindings);
    fireEvent.click(screen.getByRole("option", { name: /^New Shell/ }));
    fireEvent.change(screen.getByRole("textbox", { name: "Shortcut" }), {
      target: { value: "Alt+F2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save shortcut" }));
    await screen.findByText("changed on disk");
    expect(
      (screen.getByRole("textbox", { name: "Shortcut" }) as HTMLInputElement)
        .value,
    ).toBe("Alt+F2");
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    shortcut("KeyN");
    expect(
      screen.getByRole("dialog", { name: "New shell — host · 1/2" }),
    ).toBeTruthy();
  });

  it("keeps defaults usable when loading shortcut settings fails, without overwriting the file", async () => {
    api.loadKeybindings.mockRejectedValue(
      new Error("invalid keybindings.json"),
    );
    render(<TerminalPage />);
    await screen.findByText(/Keyboard shortcuts: invalid keybindings.json/);
    shortcut("KeyN");
    expect(
      screen.getByRole("dialog", { name: "New shell — host · 1/2" }),
    ).toBeTruthy();
    expect(api.saveKeybindings).not.toHaveBeenCalled();
  });

  it("opens close confirmation with native Cmd+E, then confirms once with Cmd+E", async () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
    let finishKill!: () => void;
    api.killSession.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finishKill = resolve;
        }),
    );
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    // Queued events before the dialog renders must only open confirmation.
    nativeCommand(COMMAND_IDS.close, 2);
    const dialog = screen.getByRole("dialog", { name: "Terminate session" });
    expect(dialog.textContent).toContain("Press ⌘E to confirm");
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "Cancel" }),
    );
    expect(api.killSession).not.toHaveBeenCalled();
    nativeCommand(COMMAND_IDS.disconnect);
    shortcut("KeyN");
    shortcut("KeyP");
    expect(screen.getAllByRole("dialog")).toEqual([dialog]);
    expect(attachment.detach).not.toHaveBeenCalled();
    // Confirmation is consumed synchronously, even before React rerenders.
    nativeCommand(COMMAND_IDS.close, 2);
    nativeCommand(COMMAND_IDS.close);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.killSession).toHaveBeenCalledExactlyOnceWith({
      target: { kind: "ssh", destination: "test", host_id: "test-id", host_name: "test", method_id: "default" },
      session_id: "known-id",
    });
    await act(async () => finishKill());
  });

  it("confirms only the non-active session named by the sidebar close dialog", async () => {
    const saved = snapshot();
    saved.document.sessions.push({
      host_id: "test-id",
      session_id: "other-id",
      name: "other",
      last_known_cwd: null,
      last_known_cwd_display: null,
    });
    api.loadWorkspace.mockResolvedValue(saved);
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: "Terminate other" }));
    expect(screen.getByRole("dialog").textContent).toContain(
      "Terminate other for all clients?",
    );
    nativeCommand(COMMAND_IDS.close);
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Terminate other" })).toBeNull(),
    );
    expect(api.killSession).toHaveBeenCalledExactlyOnceWith({
      target: { kind: "ssh", destination: "test", host_id: "test-id", host_name: "test", method_id: "default" },
      session_id: "other-id",
    });
    expect(
      screen.getByRole("button", { name: "Terminate remembered" }),
    ).toBeTruthy();
  });

  it("cancels native close confirmation with Escape and requires a new confirmation", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    nativeCommand(COMMAND_IDS.close);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    nativeCommand(COMMAND_IDS.close);
    expect(screen.getByRole("dialog", { name: "Terminate session" })).toBeTruthy();
    expect(api.killSession).not.toHaveBeenCalled();
  });

  it("requires a fresh Ctrl+Shift+E key press, ignoring held repeats and composition", async () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("Linux");
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    shortcut("KeyE");
    const dialog = screen.getByRole("dialog", { name: "Terminate session" });
    expect(dialog.textContent).toContain("Press Ctrl+Shift+E to confirm");
    fireEvent.keyDown(dialog, {
      code: "KeyE",
      ctrlKey: true,
      shiftKey: true,
      repeat: true,
    });
    fireEvent.keyDown(dialog, {
      code: "KeyE",
      ctrlKey: true,
      shiftKey: true,
      isComposing: true,
    });
    expect(api.killSession).not.toHaveBeenCalled();
    shortcut("KeyE");
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Terminate remembered" }),
      ).toBeNull(),
    );
    expect(api.killSession).toHaveBeenCalledOnce();
  });

  it("does not route native close into a different confirmation dialog", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(
      screen.getByRole("button", { name: "Remove remembered from workspace" }),
    );
    const dialog = screen.getByRole("dialog");
    nativeCommand(COMMAND_IDS.close);
    nativeCommand(COMMAND_IDS.close);
    expect(screen.getAllByRole("dialog")).toEqual([dialog]);
    expect(api.killSession).not.toHaveBeenCalled();
  });

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
    nativeCommand(COMMAND_IDS.close);
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
      host_name: "test",
      method_id: "default",
    };
    const verified = { ...target, remote_info: remoteInfo };
    const created = newSession(verified);
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
      target: verified,
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
    expect(api.probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function));
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
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

  it("keeps unused SSH config projections out of the sidebar while offering them in pickers", async () => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    expect(screen.queryByRole("button", { name: "Host settings for only-in-ssh-config" })).toBeNull();
    expect(screen.queryByRole("region", { name: "only-in-ssh-config sessions" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    expect(screen.getByRole("option", { name: "only-in-ssh-config" })).toBeTruthy();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(screen.queryByRole("region", { name: "only-in-ssh-config sessions" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Remove only-in-ssh-config" })).toBeNull();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  async function chooseProjectedConnection() {
    await screen.findByRole("button", { name: "Host settings for test" });
    shortcut("KeyP");
    fireEvent.change(screen.getByRole("combobox", { name: "Search commands" }), { target: { value: "Connect Host" } });
    fireEvent.click(screen.getByRole("option", { name: /Connect Host/ }));
    const picker = await screen.findByRole("dialog", { name: "Connect host" });
    fireEvent.click(within(picker).getByRole("option", { name: "only-in-ssh-config" }));
  }

  it.each([
    ["Add host", false],
    ["Choose host to connect", true],
    ["New shell", true],
    ["Add existing session", true],
  ] as const)("labels virtual projections explicitly in the %s selector", async (button, includesSaved) => {
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    fireEvent.click(screen.getByRole("button", { name: button }));
    const virtual = screen.getByRole("group", { name: "SSH config · Virtual" });
    expect(within(virtual).getByRole("option", { name: "only-in-ssh-config" })).toBeTruthy();
    if (includesSaved) {
      const saved = screen.getByRole("group", { name: "Saved hosts" });
      expect(within(saved).getByRole("option", { name: "test" })).toBeTruthy();
      expect(within(saved).queryByRole("option", { name: "only-in-ssh-config" })).toBeNull();
    }
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
  });

  it("reveals a projected host after a successful connection without saving its definition", async () => {
    render(<TerminalPage />);
    await chooseProjectedConnection();
    expect(screen.queryByRole("region", { name: "only-in-ssh-config sessions" })).toBeNull();
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    expect(await screen.findByRole("status", { name: "Host connection for only-in-ssh-config: Connected" })).toBeTruthy();
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(api.updateHosts).not.toHaveBeenCalled();
    for (const [, saved] of api.updateWorkspace.mock.calls) {
      expect(saved.hosts).toBeUndefined();
      expect(saved.host_identities).not.toContainEqual(expect.objectContaining({ host_id: projectedHostId("only-in-ssh-config") }));
    }
  });

  it("keeps a projection hidden after a failed connection", async () => {
    api.probeSshHost.mockRejectedValueOnce(new Error("Connection refused"));
    render(<TerminalPage />);
    await chooseProjectedConnection();
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await screen.findByRole("dialog", { name: "Could not connect" });
    expect(screen.queryByRole("region", { name: "only-in-ssh-config sessions" })).toBeNull();
    expect(api.updateHosts).not.toHaveBeenCalled();
  });

  it("keeps a cancelled projection hidden even if verification finishes later", async () => {
    let finish!: (identity: typeof remoteInfo) => void;
    api.probeSshHost.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    render(<TerminalPage />);
    await chooseProjectedConnection();
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await act(async () => finish(remoteInfo));
    expect(screen.queryByRole("region", { name: "only-in-ssh-config sessions" })).toBeNull();
    expect(api.cancelSshProbe).toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
  });

  it.each([false, true])("pins a projected host before creating its first shell (promoted: %s)", async (promoted) => {
    const host_id = projectedHostId("only-in-ssh-config");
    if (promoted) {
      const catalog = hostSnapshot();
      catalog.document.hosts.push({
        host_id, name: "only-in-ssh-config", preferred_method_id: "ssh_config",
        connection_methods: [{ method_id: "ssh_config", name: "SSH config", target: { kind: "ssh", destination: "only-in-ssh-config" } }],
      });
      api.loadHosts.mockResolvedValue(catalog);
    }
    let finish!: (identity: typeof remoteInfo) => void;
    api.probeSshHost.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    api.createSession.mockImplementation(async (request: { target: ConnectionTarget }) => newSession(request.target));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    fireEvent.click(screen.getByRole("option", { name: "only-in-ssh-config" }));
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    expect(api.createSession).not.toHaveBeenCalled();
    await act(async () => finish(remoteInfo));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.createSession).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      target: expect.objectContaining({ host_id, remote_info: remoteInfo }),
    }));
    const saved = api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument;
    expect(saved.host_identities).toContainEqual({ host_id, remote_info: remoteInfo });
    expect(saved.sessions).toContainEqual(expect.objectContaining({ host_id, session_id: "created-id" }));
    if (!promoted) expect(api.updateHosts).not.toHaveBeenCalled();
    expect(await screen.findByRole("button", { name: "Disconnect host only-in-ssh-config" })).toBeTruthy();
  });

  it.each(["session", "task", "forward"])("keeps SSH config projections with a %s reference visible", async (reference) => {
    const host_id = projectedHostId("only-in-ssh-config");
    const saved = snapshot();
    if (reference === "session") {
      saved.document.sessions[0].host_id = host_id;
      saved.document.tabs = [{ host_id, session_id: "known-id" }];
      saved.document.active_tab = saved.document.tabs[0];
    } else if (reference === "task") {
      saved.document.task_references = [{ host_id, task_id: "remote-task", definition_id: null, applied_revision: null, is_default: false }];
    } else {
      saved.document.port_forwards = [{ host_id, forward_id: "remote-port", name: "Web", enabled: false, bind_address: "127.0.0.1", local_port: 8080, remote_host: "localhost", remote_port: 80 }];
    }
    api.loadWorkspace.mockResolvedValue(saved);
    render(<TerminalPage />);
    expect(await screen.findByRole("button", { name: "Host settings for only-in-ssh-config" })).toBeTruthy();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
  });

  it("verifies a projected host before discovery and uses its identity when importing", async () => {
    const host_id = projectedHostId("only-in-ssh-config");
    let finish!: (identity: typeof remoteInfo) => void;
    api.probeSshHost.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    api.listSessions.mockImplementation(async (target: ConnectionTarget) => ({ sessions: [newSession(target)], shell_states: {} }));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    fireEvent.click(screen.getByRole("button", { name: "Add existing session" }));
    fireEvent.click(screen.getByRole("option", { name: "only-in-ssh-config" }));
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    await act(async () => finish(remoteInfo));
    fireEvent.click(await screen.findByRole("option", { name: /created-shell/ }));
    await waitFor(() => expect(api.updateWorkspace.mock.calls.some(([, saved]) =>
      saved.host_identities?.some((identity: { host_id: string }) => identity.host_id === host_id))).toBe(true));
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(api.listSessions).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ host_id, remote_info: remoteInfo }));
    expect(api.probeSshHost.mock.invocationCallOrder[0]).toBeLessThan(api.listSessions.mock.invocationCallOrder[0]);
    expect(api.updateHosts).not.toHaveBeenCalled();
    const saved = api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument;
    expect(saved.host_identities).toContainEqual({ host_id, remote_info: remoteInfo });
    expect(saved.sessions).toContainEqual(expect.objectContaining({ host_id, session_id: "created-id" }));
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("pins a projected host before adding a forward definition", async () => {
    const host_id = projectedHostId("only-in-ssh-config");
    const savedWorkspace = snapshot();
    savedWorkspace.document.sessions[0].host_id = host_id;
    savedWorkspace.document.tabs = [{ host_id, session_id: "known-id" }];
    savedWorkspace.document.active_tab = { host_id, session_id: "known-id" };
    api.loadWorkspace.mockResolvedValue(savedWorkspace);
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Port forwarding for only-in-ssh-config" }));
    const dialog = await screen.findByRole("dialog", { name: "Port forwarding" });
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    fireEvent.change(within(dialog).getByLabelText("Name"), { target: { value: "Web" } });
    fireEvent.change(within(dialog).getByLabelText("Local port"), { target: { value: "8080" } });
    fireEvent.change(within(dialog).getByLabelText("Remote port"), { target: { value: "80" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add stopped forward" }));
    await waitFor(() => expect(api.updateWorkspace.mock.calls.some(([, saved]) => saved.port_forwards?.length === 1)).toBe(true));
    const saved = api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument;
    expect(saved.host_identities).toContainEqual({ host_id, remote_info: remoteInfo });
    expect(saved.port_forwards).toContainEqual(expect.objectContaining({ host_id, enabled: false }));
    expect(api.updateHosts).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Start" }));
    await waitFor(() => expect(api.configurePortForward).toHaveBeenCalledWith(
      expect.objectContaining({ host_id, remote_info: remoteInfo }), expect.anything(), true,
    ));
  });

  it("answers first-use SSH credentials and resumes the requested new shell", async () => {
    let showPrompt!: (prompt: SshPrompt) => void;
    let finish!: (identity: typeof remoteInfo) => void;
    api.probeSshHost.mockImplementationOnce((_target, _attempt, prompt) => {
      showPrompt = prompt;
      return new Promise((resolve) => { finish = resolve; });
    });
    api.createSession.mockImplementation(async (request: { target: ConnectionTarget }) => newSession(request.target));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    fireEvent.click(screen.getByRole("option", { name: "only-in-ssh-config" }));
    fireEvent.change(screen.getByLabelText("Working directory"), { target: { value: "/project/work" } });
    expect(api.probeSshHost).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    await act(async () => showPrompt({ prompt_id: "password", kind: "secret", message: "Password:" }));
    const input = screen.getByLabelText("SSH response");
    expect(input).toHaveProperty("type", "password");
    fireEvent.change(input, { target: { value: "test-only-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(api.respondSshPrompt).toHaveBeenCalledExactlyOnceWith(
      api.probeSshHost.mock.calls[0][1], "password", "test-only-secret",
    );
    expect(api.cancelSshProbe).not.toHaveBeenCalled();
    expect(api.createSession).not.toHaveBeenCalled();
    await act(async () => finish(remoteInfo));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.createSession).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      target: expect.objectContaining({ host_id: projectedHostId("only-in-ssh-config"), remote_info: remoteInfo }),
      working_directory: "/project/work",
    }));
    expect(attachment.connect).toHaveBeenCalledOnce();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.listSessions).not.toHaveBeenCalled();
  });

  it.each([
    ["New shell", "save"],
    ["New shell", "refresh"],
    ["Add existing session", "save"],
    ["Add existing session", "refresh"],
  ])("does not continue %s when native close overlaps the verification %s", async (action, stage) => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    let finish!: () => void;
    if (stage === "save") {
      api.updateHosts.mockImplementationOnce((_revision: string | null, document: HostCatalogDocument) =>
        new Promise((resolve) => { finish = () => resolve({ revision: "verified-host", document }); }));
    } else {
      api.listPortForwards.mockImplementationOnce(() =>
        new Promise((resolve) => { finish = () => resolve([]); }));
    }
    api.createSession.mockImplementation(async (request: { target: ConnectionTarget }) => newSession(request.target));
    api.listSessions.mockImplementation(async () => ({ sessions: [], shell_states: {} }));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    await waitFor(() => expect(nativeWindow.onCloseRequested).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: action }));
    fireEvent.click(screen.getByRole("option", { name: "test" }));
    if (action === "New shell") fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(stage === "save" ? api.updateHosts : api.listPortForwards).toHaveBeenCalledOnce());
    const preventDefault = vi.fn();
    let closing!: Promise<void>;
    act(() => { closing = nativeWindow.onCloseRequested.mock.calls[0][0]({ preventDefault }); });
    expect(preventDefault).toHaveBeenCalledOnce();
    await act(async () => {
      finish();
      await closing;
    });
    await screen.findByText("Workspace is closing.");
    expect(nativeWindow.destroy).toHaveBeenCalledOnce();
    expect(api.createSession).not.toHaveBeenCalled();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    if (stage === "save") expect(api.listPortForwards).not.toHaveBeenCalled();
  });

  it.each(["Connect host", "New shell", "Add existing session"])("preserves verified batch SSH for %s when broker status is unsupported", async (action) => {
    const unsupported = {
      code: "ssh_broker_unsupported",
      message: "SSH connection status requires macOS or Linux.",
    };
    api.sshConnectionStatus.mockRejectedValue(unsupported);
    api.listPortForwards.mockRejectedValue(unsupported);
    api.createSession.mockImplementation(async (request: { target: ConnectionTarget }) => newSession(request.target));
    api.listSessions.mockResolvedValue({ sessions: [], shell_states: {} });
    const known = restoreWorkspace(snapshot().document, hostSnapshot().document).sessions[0];
    api.inspectKnownSessions.mockResolvedValue([{
      session_id: known.session_id,
      session: { ...known, status: "running" },
      shell_state: null,
      error: null,
    }]);
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: action }));
    fireEvent.click(screen.getByRole("option", { name: action === "Connect host" ? "Connect" : "test" }));
    if (action === "New shell") fireEvent.click(screen.getByRole("button", { name: "Create shell" }));

    if (action === "Add existing session") {
      await waitFor(() => expect(api.listSessions).toHaveBeenCalledOnce());
    } else {
      await waitFor(() => expect(attachment.connect).toHaveBeenCalledOnce());
      expect(api.createSession).toHaveBeenCalledTimes(action === "New shell" ? 1 : 0);
    }
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(screen.queryByRole("dialog", { name: "Could not connect" })).toBeNull();
  });

  it("does not bypass a failed live status check when the broker is supported", async () => {
    api.sshConnectionStatus.mockRejectedValue({ code: "ctld_protocol_error", message: "ctld status failed" });
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Connect host" }));
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await screen.findByRole("dialog", { name: "Could not connect" });
    expect(screen.getByText("ctld status failed")).toBeTruthy();
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("retains the directory after cancelled authentication and ignores late completion", async () => {
    let showPrompt!: (prompt: SshPrompt) => void;
    let finish!: (identity: typeof remoteInfo) => void;
    api.probeSshHost.mockImplementationOnce((_target, _attempt, prompt) => {
      showPrompt = prompt;
      return new Promise((resolve) => { finish = resolve; });
    });
    api.createSession.mockImplementation(async (request: { target: ConnectionTarget }) => newSession(request.target));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    fireEvent.click(screen.getByRole("button", { name: /New shell/ }));
    fireEvent.click(screen.getByRole("option", { name: "only-in-ssh-config" }));
    fireEvent.change(screen.getByLabelText("Working directory"), { target: { value: "/keep/this-draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Cancel quick input" }));
    expect(await screen.findByLabelText("Working directory")).toHaveProperty("value", "/keep/this-draft");
    expect(api.cancelSshProbe).toHaveBeenCalledExactlyOnceWith(api.probeSshHost.mock.calls[0][1]);
    await act(async () => {
      showPrompt({ prompt_id: "late", kind: "secret", message: "Stale password request" });
      finish(remoteInfo);
    });
    expect(screen.queryByLabelText("SSH response")).toBeNull();
    expect(api.createSession).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.probeSshHost).toHaveBeenCalledTimes(2);
    expect(api.createSession).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ working_directory: "/keep/this-draft" }));
  });

  it("returns to host selection after cancelled discovery authentication without listing sessions", async () => {
    let showPrompt!: (prompt: SshPrompt) => void;
    let finish!: (identity: typeof remoteInfo) => void;
    api.probeSshHost.mockImplementationOnce((_target, _attempt, prompt) => {
      showPrompt = prompt;
      return new Promise((resolve) => { finish = resolve; });
    });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    fireEvent.click(screen.getByRole("button", { name: "Add existing session" }));
    fireEvent.click(screen.getByRole("option", { name: "only-in-ssh-config" }));
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    await act(async () => showPrompt({ prompt_id: "password", kind: "secret", message: "Password:" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel quick input" }));
    await screen.findByRole("dialog", { name: "Add existing session — host" });
    await act(async () => {
      showPrompt({ prompt_id: "late", kind: "secret", message: "Stale password request" });
      finish(remoteInfo);
    });
    expect(screen.getByRole("option", { name: "only-in-ssh-config" })).toBeTruthy();
    expect(screen.queryByLabelText("SSH response")).toBeNull();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.createSession).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
  });

  it.each(["New shell", "Add existing session"])("reconnects a saved pinned host before %s", async (action) => {
    const catalog = hostSnapshot();
    catalog.document.hosts[0].remote_info = remoteInfo;
    api.loadHosts.mockResolvedValue(catalog);
    let finish!: (identity: typeof remoteInfo) => void;
    api.probeSshHost.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    api.createSession.mockImplementation(async (request: { target: ConnectionTarget }) => newSession(request.target));
    api.listSessions.mockImplementation(async (target: ConnectionTarget) => ({ sessions: [newSession(target)], shell_states: {} }));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    expect(api.probeSshHost).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: action }));
    fireEvent.click(screen.getByRole("option", { name: "test" }));
    if (action === "New shell") {
      expect(api.probeSshHost).not.toHaveBeenCalled();
      fireEvent.click(screen.getByRole("button", { name: "Create shell" }));
    }
    await waitFor(() => expect(api.probeSshHost).toHaveBeenCalledOnce());
    const target = {
      kind: "ssh", destination: "test", host_id: "test-id", host_name: "test", method_id: "default", remote_info: remoteInfo,
    };
    expect(api.probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function));
    expect(api.createSession).not.toHaveBeenCalled();
    expect(api.listSessions).not.toHaveBeenCalled();
    await act(async () => finish(remoteInfo));
    if (action === "New shell") {
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      expect(api.createSession).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ target }));
    } else {
      fireEvent.click(await screen.findByRole("option", { name: /created-shell/ }));
      await waitFor(() => expect(api.updateWorkspace.mock.calls.some(([, saved]) =>
        saved.sessions.some((session: { session_id: string }) => session.session_id === "created-id"))).toBe(true));
      expect(api.listSessions).toHaveBeenCalledExactlyOnceWith(target);
      expect(api.createSession).not.toHaveBeenCalled();
      expect(attachment.connect).not.toHaveBeenCalled();
    }
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
  });

  it("saves a customized SSH config projection while preserving its remembered session references", async () => {
    const host_id = projectedHostId("only-in-ssh-config");
    const saved = snapshot();
    saved.document.sessions[0].host_id = host_id;
    saved.document.tabs = [{ host_id, session_id: "known-id" }];
    saved.document.active_tab = { host_id, session_id: "known-id" };
    api.loadWorkspace.mockResolvedValue(saved);
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Host settings for only-in-ssh-config" }));
    fireEvent.change(screen.getByLabelText("Host name"), { target: { value: "Office machine" } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("button", { name: "Host settings for Office machine" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Host settings for only-in-ssh-config" })).toBeNull();
    expect(api.updateHosts).toHaveBeenCalledOnce();
    const catalog = api.updateHosts.mock.calls[0][1] as HostCatalogDocument;
    expect(catalog.hosts).toHaveLength(3);
    expect(catalog.hosts[2]).toMatchObject({
      host_id, name: "Office machine", preferred_method_id: "ssh_config",
      connection_methods: [{ method_id: "ssh_config", target: { kind: "ssh", destination: "only-in-ssh-config" } }],
    });
    const document = api.updateWorkspace.mock.calls.slice(-1)[0][1] as WorkspaceDocument;
    expect(document.sessions).toEqual(saved.document.sessions);
    expect(document.active_tab).toEqual({ kind: "session", host_id, session_id: "known-id" });
    expect(document.hosts).toBeUndefined();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("adds an address, name, and credentials before saving the host without merging another remote environment", async () => {
    const saved = snapshot();
    const catalog = hostSnapshot();
    catalog.document.hosts[0].remote_info = remoteInfo;
    api.loadHosts.mockResolvedValue(catalog);
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Add host" }));
    fireEvent.change(screen.getByLabelText("SSH host"), { target: { value: "deploy@build.example:2222" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(screen.getByLabelText("Host name"), {
      target: { value: "Build server at home" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.queryByLabelText("Method name")).toBeNull();
    fireEvent.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("button", { name: "Host settings for Build server at home" })).toBeTruthy();

    const persisted = api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument;
    const hosts = api.updateHosts.mock.calls.slice(-1)[0]![1] as HostCatalogDocument;
    const host = hosts.hosts[2];
    expect(hosts.hosts).toHaveLength(3);
    expect(host).toMatchObject({ name: "Build server at home", remote_info: remoteInfo });
    expect(host.host_id).not.toBe("test-id");
    expect(host.connection_methods).toEqual([{
      method_id: expect.any(String), name: "SSH",
      target: { kind: "ssh", destination: "build.example", hostname: "build.example", user: "deploy", port: 2222 },
    }]);
    expect(host.preferred_method_id).toBe(host.connection_methods[0].method_id);
    expect(hosts.hosts[0]).toEqual(catalog.document.hosts[0]);
    expect(persisted.hosts).toBeUndefined();
    expect(persisted.sessions).toEqual(saved.document.sessions);
    expect(persisted.active_tab).toEqual({ kind: "session", host_id: "test-id", session_id: "known-id" });
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(attachment.detach).not.toHaveBeenCalled();
  });

  it("adds a connection without touching the active session, then explicitly switches through the saved method", async () => {
    const saved = snapshot();
    const catalog = hostSnapshot();
    catalog.document.hosts[0].remote_info = remoteInfo;
    api.loadHosts.mockResolvedValue(catalog);
    saved.document.port_forwards = [{
      host_id: "test-id", forward_id: "web", name: "Web", enabled: true,
      bind_address: "127.0.0.1", local_port: 8080, remote_host: "127.0.0.1", remote_port: 80,
    }];
    api.loadWorkspace.mockResolvedValue(saved);
    const known = restoreWorkspace(saved.document, catalog.document).sessions[0];
    api.inspectKnownSessions.mockImplementation(async (target: ConnectionTarget) => [{
      session_id: known.session_id,
      session: { ...known, target, status: "running", next_sequence: "42" },
      shell_state: null, error: null,
    }]);
    Object.assign(attachment.state, { phase: "attached", session: known } satisfies Partial<AttachmentViewState>);
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Host settings for test" });
    await waitFor(() => expect(api.listPortForwards).toHaveBeenCalledOnce());
    api.configurePortForward.mockClear();
    api.listPortForwards.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Host settings for test" }));
    fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    fireEvent.change(screen.getByLabelText("Method name"), { target: { value: "VPN" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(screen.getByLabelText("SSH host or config alias"), {
      target: { value: "only-in-ssh-config" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Verify and save" }));
    const settings = await screen.findByRole("dialog", { name: "Host settings · test" });
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(attachment.detach).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(api.configurePortForward).not.toHaveBeenCalled();

    const persisted = api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument;
    const hosts = api.updateHosts.mock.calls.slice(-1)[0]![1] as HostCatalogDocument;
    const host = hosts.hosts[0];
    expect(hosts.hosts).toHaveLength(2);
    expect(host).toMatchObject({ host_id: "test-id", name: "test", preferred_method_id: "default", remote_info: remoteInfo });
    expect(host.connection_methods).toEqual([
      { method_id: "default", name: "SSH", target: { kind: "ssh", destination: "test" } },
      { method_id: expect.any(String), name: "VPN", target: { kind: "ssh", destination: "only-in-ssh-config" } },
    ]);
    expect(persisted.sessions).toEqual(saved.document.sessions);
    expect(persisted.active_tab).toEqual({ kind: "session", host_id: "test-id", session_id: "known-id" });
    expect(persisted.port_forwards).toEqual(saved.document.port_forwards);

    fireEvent.click(within(within(settings).getByRole("region", { name: "VPN" })).getByRole("button", { name: "Connect using" }));
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledOnce());
    const expected = {
      kind: "ssh", host_id: "test-id", host_name: "test", method_id: host.connection_methods[1].method_id,
      destination: "only-in-ssh-config", remote_info: remoteInfo,
    };
    expect(attachment.connect.mock.calls[0][0]).toMatchObject({ target: expected, session_id: "known-id" });
    expect(api.inspectKnownSessions).toHaveBeenCalledExactlyOnceWith(expected, ["known-id"]);
    expect(api.configurePortForward.mock.calls).toEqual([
      [expected, saved.document.port_forwards[0], true],
    ]);
    expect(api.listPortForwards).toHaveBeenCalledExactlyOnceWith(expected);
    const reloaded = restoreWorkspace(api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument, hosts);
    expect(reloaded.hosts[1].preferred_method_id).toBe("default");
    expect(reloaded.sessions[0].session_id).toBe("known-id");
  });

  it("retries a failed new-host save without adding duplicate hosts", async () => {
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Add host" }));
    fireEvent.change(screen.getByLabelText("SSH host"), { target: { value: "build.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(screen.getByLabelText("Host name"), { target: { value: "Build machine" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    api.updateHosts.mockRejectedValueOnce(new Error("disk full"));
    fireEvent.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await screen.findByRole("dialog", { name: "Could not save host" });
    expect(screen.queryByRole("button", { name: "Host settings for Build machine" })).toBeNull();
    fireEvent.click(screen.getByRole("option", { name: "Retry saving host" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("button", { name: "Host settings for Build machine" })).toBeTruthy();
    const document = api.updateHosts.mock.calls.slice(-1)[0][1] as HostCatalogDocument;
    expect(document.hosts).toHaveLength(3);
    const added = document.hosts.filter((host) => host.name === "Build machine");
    expect(added).toHaveLength(1);
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("retries a failed method save without retaining the failed method in workspace state", async () => {
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Host settings for test" }));
    fireEvent.click(screen.getByRole("button", { name: "Add connection" }));
    fireEvent.change(screen.getByLabelText("Method name"), { target: { value: "VPN" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.change(screen.getByLabelText("SSH host or config alias"), { target: { value: "vpn.example" } });
    api.updateHosts.mockRejectedValueOnce(new Error("disk full"));
    fireEvent.click(screen.getByRole("button", { name: "Verify and save" }));
    await screen.findByRole("dialog", { name: "Could not connect" });
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await screen.findByRole("dialog", { name: "Host settings · test" });
    const document = api.updateHosts.mock.calls.slice(-1)[0][1] as HostCatalogDocument;
    const host = document.hosts[0];
    expect(document.hosts).toHaveLength(2);
    expect(host.connection_methods).toHaveLength(2);
    expect(host.connection_methods.filter((method) => method.name === "VPN")).toHaveLength(1);
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("keeps host settings draft retryable after a failed save", async () => {
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Host settings for test" }));
    fireEvent.change(screen.getByLabelText("Host name"), { target: { value: "Build machine" } });
    api.updateHosts.mockRejectedValueOnce(new Error("disk full"));
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Save changes" })).toHaveProperty("disabled", false));
    expect(screen.getByRole("dialog", { name: "Host settings · test" })).toBeTruthy();
    expect(screen.getByLabelText("Host name")).toHaveProperty("value", "Build machine");
    expect(screen.getByRole("button", { name: "Host settings for test" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("button", { name: "Host settings for Build machine" })).toBeTruthy();
    expect(api.updateHosts.mock.calls.slice(-1)[0][1].hosts[0].name).toBe("Build machine");
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("renames a host and changes its preference without reconnecting, then Connect uses that preference", async () => {
    const saved = snapshot().document;
    const catalog = hostSnapshot();
    catalog.document.hosts[0].remote_info = remoteInfo;
    catalog.document.hosts[0].connection_methods.push({
      method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "vpn.example" },
    });
    api.loadWorkspace.mockResolvedValue({ revision: "one", document: saved });
    api.loadHosts.mockResolvedValue(catalog);
    const known = restoreWorkspace(saved, catalog.document).sessions[0];
    Object.assign(attachment.state, { phase: "attached", session: known } satisfies Partial<AttachmentViewState>);
    api.inspectKnownSessions.mockImplementation(async (target: ConnectionTarget) => [{
      session_id: known.session_id,
      session: { ...known, target, status: "running" }, shell_state: null, error: null,
    }]);
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Host settings for test" }));
    fireEvent.change(screen.getByLabelText("Host name"), { target: { value: "Build server" } });
    fireEvent.click(within(screen.getByRole("region", { name: "VPN" })).getByRole("button", { name: "Make preferred" }));
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("button", { name: "Host settings for Build server" })).toBeTruthy();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(attachment.detach).not.toHaveBeenCalled();
    const persisted = api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument;
    expect(api.updateHosts.mock.calls.slice(-1)[0][1].hosts[0]).toMatchObject({ host_id: "test-id", name: "Build server", preferred_method_id: "vpn" });
    expect(persisted.sessions).toEqual(saved.sessions);
    expect(persisted.active_tab).toEqual({ ...saved.active_tab, kind: "session" });

    fireEvent.click(screen.getByRole("button", { name: "Choose host to connect" }));
    fireEvent.click(screen.getByRole("option", { name: "Build server" }));
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledOnce());
    const expected = {
      kind: "ssh", host_id: "test-id", host_name: "Build server", method_id: "vpn",
      destination: "vpn.example", remote_info: remoteInfo,
    };
    expect(api.probeSshHost.mock.calls[0][0]).toEqual(expected);
    expect(api.inspectKnownSessions).toHaveBeenCalledExactlyOnceWith(expected, ["known-id"]);
    expect(attachment.connect.mock.calls[0][0]).toMatchObject({ target: expected, session_id: "known-id" });
    expect(api.killSession).not.toHaveBeenCalled();
  });

  it.each([false, true])("removes only unshared host credentials (gateway route: %s)", async (routed) => {
    const catalog = hostSnapshot();
    const document = catalog.document;
    document.hosts[1].connection_methods[0].target = { kind: "ssh", destination: "test", user: "another-account" };
    if (routed) {
      document.ssh_gateways = [
        { gateway_id: "edge-a", name: "Edge A", destination: "edge.example", user: "deploy", port: 2222 },
        { gateway_id: "edge-b", name: "Edge B", destination: "edge.example", user: "deploy", port: 2222 },
      ];
      document.hosts[0].connection_methods[0].target.gateway_route = [{ gateway_id: "edge-a", mode: "native_only" }];
      document.hosts[1].connection_methods[0].target.gateway_route = [{ gateway_id: "edge-b", mode: "native_only" }];
    }
    document.hosts[0].connection_methods.push({
      method_id: "private", name: "Private method", target: { kind: "ssh", destination: "private.example" },
    });
    api.loadHosts.mockResolvedValue(catalog);
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove test" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Remove test" })).toBeNull());
    expect(api.forgetSshCredentials).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      host_id: "test-id", destination: "private.example", method_id: "private",
    }));
    expect(screen.getByRole("button", { name: "Remove unused" })).toBeTruthy();
    expect(api.killSession).not.toHaveBeenCalled();
  });

  it("deletes an unshared credential scope once when removing its only host", async () => {
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove test" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Remove test" })).toBeNull());
    expect(api.forgetSshCredentials).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      host_id: "test-id", destination: "test",
    }));
  });

  it("keeps a restored remote tab cold, then resumes it after connecting its host", async () => {
    const known = restoreWorkspace(snapshot().document, hostSnapshot().document).sessions[0];
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
        new Promise<typeof remoteInfo>((resolve) => {
          authenticated = () => resolve(remoteInfo);
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
      { kind: "ssh", destination: "test", host_id: "test-id", host_name: "test", method_id: "default", remote_info: remoteInfo },
      ["known-id"],
    );
    expect(api.listSessions).not.toHaveBeenCalled();
  });

  it("shows live host connection methods without probing or persisting status", async () => {
    const catalog = hostSnapshot();
    catalog.document.hosts[0].connection_methods.push({
      method_id: "vpn", name: "VPN", target: { kind: "ssh", destination: "vpn.example" },
    });
    catalog.document.hosts[1].remote_info = remoteInfo;
    api.loadHosts.mockResolvedValue(catalog);
    api.sshConnectionStatus.mockImplementation(async (target: ConnectionTarget) => ({
      connected: target.kind === "ssh" && target.destination === "vpn.example",
      manually_disconnected: false,
    }));
    render(<TerminalPage />);

    const connected = await screen.findByRole("status", { name: "Host connection for test: Connected" });
    expect(connected.title).toBe("Connected\nConnection methods: VPN");
    expect(screen.getByRole("button", { name: "Disconnect host test" })).toBeTruthy();
    expect(screen.getByRole("status", { name: "Host connection for unused: Disconnected" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Connect to unused" })).toBeTruthy();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
  });

  it("disconnects a host while preserving its tab, sessions, and enabled forwards, then resumes explicitly", async () => {
    const saved = snapshot();
    const forward: WorkspacePortForward = {
      host_id: "test-id", forward_id: "web", name: "Web", enabled: true,
      bind_address: "127.0.0.1", local_port: 8080, remote_host: "localhost", remote_port: 80,
    };
    saved.document.port_forwards = [forward];
    api.loadWorkspace.mockResolvedValue(saved);
    const known = restoreWorkspace(saved.document, hostSnapshot().document).sessions[0];
    Object.assign(attachment.state, { phase: "attached", session: known } satisfies Partial<AttachmentViewState>);
    attachment.detach.mockImplementationOnce(async () => {
      attachment.state.phase = "idle";
      attachment.state.session = null;
    });
    let manuallyDisconnected = false;
    let finishDisconnect!: () => void;
    api.sshConnectionStatus.mockImplementation(async (target: ConnectionTarget) => ({
      connected: target.kind === "ssh" && (target.host_id !== "test-id" || !manuallyDisconnected),
      manually_disconnected: target.kind === "ssh" && target.host_id === "test-id" && manuallyDisconnected,
    }));
    api.disconnectSshHost.mockImplementationOnce(() => new Promise<void>((resolve) => {
      finishDisconnect = () => { manuallyDisconnected = true; resolve(); };
    }));
    api.probeSshHost.mockImplementation(async () => {
      manuallyDisconnected = false;
      return remoteInfo;
    });
    api.inspectKnownSessions.mockImplementation(async (target: ConnectionTarget) => [{
      session_id: known.session_id,
      session: { ...known, target, status: "running" }, shell_state: null, error: null,
    }]);
    render(<TerminalPage />);
    const disconnect = await screen.findByRole("button", { name: "Disconnect host test" });
    await waitFor(() => expect(api.listPortForwards).toHaveBeenCalledOnce());
    const savedBefore = api.updateWorkspace.mock.calls.length;
    api.configurePortForward.mockClear();
    api.listPortForwards.mockClear();

    fireEvent.click(disconnect);
    await waitFor(() => expect(api.disconnectSshHost).toHaveBeenCalledOnce());
    expect(screen.getByRole("status", { name: "Host connection for test: Disconnecting…" })).toBeTruthy();
    expect(disconnect).toHaveProperty("disabled", true);
    expect(attachment.detach).toHaveBeenCalledOnce();
    expect(attachment.cancelPendingConnection).toHaveBeenCalledWith(known);
    expect(api.disconnectSshHost).toHaveBeenCalledWith([known.target]);
    await act(async () => finishDisconnect());

    expect(screen.getByRole("status", { name: "Host connection for test: Disconnected" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "~/work — remembered" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "~/work on test" })).toHaveProperty("ariaSelected", "true");
    expect(api.updateWorkspace).toHaveBeenCalledTimes(savedBefore);
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.killSession).not.toHaveBeenCalled();
    expect(api.forgetSshCredentials).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Refresh sessions" }));
    fireEvent.click(screen.getByRole("button", { name: "~/work — remembered" }));
    fireEvent.click(screen.getByRole("tab", { name: "Ports" }));
    await screen.findByText("Host disconnected. Connect this host to resume forwarding.");
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(attachment.reconnect).not.toHaveBeenCalled();
    expect(api.configurePortForward).not.toHaveBeenCalled();
    expect(api.listPortForwards).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("tab", { name: "Sessions" }));
    fireEvent.click(screen.getByRole("button", { name: "Connect to test" }));
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(attachment.connect).toHaveBeenCalledOnce());
    expect(await screen.findByRole("status", { name: "Host connection for test: Connected" })).toBeTruthy();
    expect(api.probeSshHost).toHaveBeenCalledOnce();
    expect(api.configurePortForward).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ host_id: "test-id" }), forward, true,
    );
    const persisted = api.updateWorkspace.mock.calls.slice(-1)[0]![1] as WorkspaceDocument;
    expect(persisted.sessions).toEqual(saved.document.sessions);
    expect(persisted.tabs).toEqual([{ kind: "session", host_id: "test-id", session_id: "known-id" }]);
    expect(persisted.port_forwards).toEqual([forward]);
    expect(api.killSession).not.toHaveBeenCalled();
    expect(api.forgetSshCredentials).not.toHaveBeenCalled();
  });

  it("disconnects a non-active host without detaching the active host", async () => {
    const saved = snapshot();
    saved.document.sessions.push({
      host_id: "unused-id", session_id: "other-id", name: "other shell",
      last_known_cwd: null, last_known_cwd_display: null,
    });
    saved.document.tabs.push({ host_id: "unused-id", session_id: "other-id" });
    api.loadWorkspace.mockResolvedValue(saved);
    const restored = restoreWorkspace(saved.document, hostSnapshot().document);
    Object.assign(attachment.state, { phase: "attached", session: restored.sessions[0] } satisfies Partial<AttachmentViewState>);
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Disconnect host unused" }));
    await screen.findByRole("status", { name: "Host connection for unused: Disconnected" });

    expect(api.disconnectSshHost).toHaveBeenCalledExactlyOnceWith([restored.sessions[1].target]);
    expect(attachment.detach).not.toHaveBeenCalled();
    expect(attachment.cancelPendingConnection).toHaveBeenCalledExactlyOnceWith(restored.sessions[1]);
    expect(screen.getByRole("tab", { name: "~/work on test" })).toHaveProperty("ariaSelected", "true");
    expect(screen.getByRole("tab", { name: "other shell on unused" })).toBeTruthy();
    expect(api.updateHosts).not.toHaveBeenCalled();
    expect(api.updateWorkspace).not.toHaveBeenCalled();
    expect(api.killSession).not.toHaveBeenCalled();
  });

  it.each([
    ["authentication", false],
    ["authentication", true],
    ["forward refresh", false],
    ["forward refresh", true],
  ] as const)("does not resume when another client disconnects during %s (master still connected: %s)", async (stage, master_still_connected) => {
    let finish!: () => void;
    let manuallyDisconnected = false;
    api.sshConnectionStatus.mockImplementation(async () => ({
      connected: master_still_connected || !manuallyDisconnected,
      manually_disconnected: manuallyDisconnected,
    }));
    if (stage === "authentication") {
      api.probeSshHost.mockImplementationOnce(() => new Promise<typeof remoteInfo>((resolve) => {
        finish = () => resolve(remoteInfo);
      }));
    } else {
      api.listPortForwards.mockImplementationOnce(() => new Promise<[]>((resolve) => {
        finish = () => resolve([]);
      }));
    }
    render(<TerminalPage />);
    fireEvent.click(await screen.findByRole("button", { name: "Connect host" }));
    fireEvent.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(stage === "authentication" ? api.probeSshHost : api.listPortForwards).toHaveBeenCalledOnce());
    manuallyDisconnected = true;
    await act(async () => finish());

    await screen.findByRole("dialog", { name: "Could not connect" });
    expect(screen.getByText("This host was disconnected. Connect again to resume.")).toBeTruthy();
    expect(api.inspectKnownSessions).not.toHaveBeenCalled();
    expect(attachment.connect).not.toHaveBeenCalled();
    expect(api.listSessions).not.toHaveBeenCalled();
    expect(api.createSession).not.toHaveBeenCalled();
    expect(api.killSession).not.toHaveBeenCalled();
    if (stage === "authentication") {
      expect(api.updateHosts).not.toHaveBeenCalled();
      expect(api.updateWorkspace).not.toHaveBeenCalled();
    }
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
        new Promise<typeof remoteInfo>((resolve) => {
          authenticated = () => resolve(remoteInfo);
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
      { kind: "ssh", destination: "test", host_id: "test-id", host_name: "test", method_id: "default" },
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
      code: "ssh_authentication_required",
      message: "Authenticate first",
    });
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh sessions" }));
    await screen.findByText("unreachable", { exact: false });
    expect(screen.getByText("Authenticate first")).toBeTruthy();
    expect(screen.queryByText("missing", { exact: false })).toBeNull();
    expect(api.killSession).not.toHaveBeenCalled();
    expect(api.probeSshHost).not.toHaveBeenCalled();
    expect(api.respondSshPrompt).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("accepts another sidebar session click while a remote attachment is pending", async () => {
    const saved = snapshot();
    saved.document.sessions.push({
      host_id: "local",
      session_id: "local-shell",
      name: "local-shell",
      last_known_cwd: null,
      last_known_cwd_display: null,
    });
    api.loadWorkspace.mockResolvedValue(saved);
    let finish_open!: () => void;
    attachment.connect.mockImplementationOnce(() => new Promise<void>((resolve) => {
      finish_open = resolve;
    }));
    render(<TerminalPage />);
    await screen.findByRole("button", { name: "Connect host" });
    fireEvent.click(screen.getByRole("button", { name: "~/work — remembered" }));
    expect(attachment.connect).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Shell — local-shell" }));
    expect(attachment.connect).toHaveBeenCalledTimes(2);
    expect(attachment.connect).toHaveBeenLastCalledWith(
      expect.objectContaining({ session_id: "local-shell" }),
      expect.anything(),
    );
    await act(async () => { finish_open(); });
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
    fireEvent.click(screen.getByRole("button", { name: "Terminate remembered" }));
    fireEvent.click(screen.getByRole("button", { name: "Terminate session" }));
    await waitFor(() =>
      expect(api.killSession).toHaveBeenCalledExactlyOnceWith({
        target: { kind: "ssh", destination: "test", host_id: "test-id", host_name: "test", method_id: "default" },
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
        kind: "session",
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
