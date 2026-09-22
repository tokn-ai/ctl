import { describe, expect, it, vi } from "vitest";
import type { ConnectionPhase, ConnectionTarget, SessionSummary } from "../../lib/types";
import {
  buildTerminalCommands,
  COMMAND_IDS,
  sessionSwitchCommandId,
} from "./terminalCommands";
import { sessionKey } from "../targets/targets";

function session(id: string): SessionSummary {
  return {
    target: { kind: "local" },
    session_id: id,
    name: id,
    status: "running",
    terminal_size: {
      columns: 80,
      rows: 24,
      pixel_width: null,
      pixel_height: null,
    },
    next_sequence: "0",
  };
}

function setup(
  activeSessionId: string | null = "first",
  shortcutPlatform: "macos" | "other" = "macos",
  currentWorkingDirectory: string | null = "/work/rmux",
  tabIds: readonly string[] = ["first", "second", "third"],
  phase: ConnectionPhase = "attached",
  attachmentSessionId: string | null = activeSessionId,
  pendingCloseSessionId: string | null = null,
  daemonRestartConfirmationPending = false,
  restartingDaemon = false,
  currentWorkingDirectoryDisplay = currentWorkingDirectory,
  closingSessionIds: readonly string[] = [],
  disconnectingSessionId: string | null = null,
) {
  const sessions = [session("first"), session("second"), session("third")];
  const tabs = sessions.filter((candidate) =>
    tabIds.includes(candidate.session_id),
  );
  const identityFor = (sessionId: string | null) =>
    sessionId === null ? null : sessionKey(session(sessionId));
  const actions = {
    showPalette: vi.fn(),
    showAddHost: vi.fn(),
    showConnectHost: vi.fn(),
    showAddRoutedHost: vi.fn(),
    showAddExistingSession: vi.fn(),
    forgetSession: vi.fn(),
    showNewShell: vi.fn(),
    openShellTab: vi.fn(),
    refreshSessions: vi.fn(),
    selectSession: vi.fn(),
    disconnectSession: vi.fn(),
    requestCloseSession: vi.fn(),
    confirmCloseSession: vi.fn(),
    toggleInput: vi.fn(),
    toggleResizeWithWindow: vi.fn(),
    reconnect: vi.fn(),
    focusTerminal: vi.fn(),
    requestDaemonRestart: vi.fn(),
    connectHost: vi.fn(),
    configureHost: vi.fn(),
    removeHost: vi.fn(),
    managePortForwards: vi.fn(),
    saveWorkspace: vi.fn(),
    configureKeybindings: vi.fn(),
    reloadKeybindings: vi.fn(),
  };
  const context: Parameters<typeof buildTerminalCommands>[0] = {
    targets: [{ kind: "local" }],
    sessions,
    tabs,
    activeSessionKey: identityFor(activeSessionId),
    attachmentSessionKey: identityFor(attachmentSessionId),
    phase,
    inputOwned: true,
    resizeWithWindow: false,
    listLoading: false,
    creating: false,
    newShellOpen: false,
    pendingCloseSessionKey: identityFor(pendingCloseSessionId),
    closingSessionKeys: new Set(
      closingSessionIds.map((id) => sessionKey(session(id))),
    ),
    disconnectingSessionKey: identityFor(disconnectingSessionId),
    terminalReady: true,
    currentWorkingDirectory,
    currentWorkingDirectoryDisplay,
    daemonRestartConfirmationPending,
    restartingDaemon,
    shortcutPlatform,
  };
  const commands = buildTerminalCommands(context, actions);
  return { sessions, tabs, actions, commands, context };
}

function findCommand(
  commands: ReturnType<typeof buildTerminalCommands>,
  id: string,
) {
  const command = commands.find((candidate) => candidate.id === id);
  expect(command).toBeDefined();
  return command!;
}

describe("terminal commands", () => {
  it.each(["first", null])("offers the host picker when %s is active and an SSH target is available", (activeSessionId) => {
    const { actions, context } = setup(activeSessionId);
    const commands = buildTerminalCommands({ ...context, targets: [
      { kind: "local" }, { kind: "ssh", host_id: "ssh-config:build", destination: "build" },
    ] }, actions);
    const command = findCommand(commands, COMMAND_IDS.connectHost);
    expect(command).toMatchObject({ title: "Connect Host", enabled: true, focusTerminalAfterRun: false });
    expect(command.isEnabled?.({})).toBe(true);
    command.run();
    expect(actions.showConnectHost).toHaveBeenCalledOnce();
    expect(actions.connectHost).not.toHaveBeenCalled();
  });

  it("connects an explicitly selected host directly without opening the picker", () => {
    const { actions, context } = setup();
    const target: ConnectionTarget = { kind: "ssh", host_id: "build", destination: "build.example" };
    const command = findCommand(buildTerminalCommands({ ...context, targets: [target] }, actions), COMMAND_IDS.connectHost);
    expect(command.isEnabled?.({ target_key: "host:build" })).toBe(true);
    command.run({ target_key: "host:build" });
    expect(actions.connectHost).toHaveBeenCalledExactlyOnceWith(target);
    expect(actions.showConnectHost).not.toHaveBeenCalled();
  });

  it("disables the picker when all targets are local or unavailable", () => {
    const { actions, context } = setup();
    const command = findCommand(buildTerminalCommands({ ...context, targets: [
      { kind: "local" }, { kind: "ssh", host_id: "missing", destination: "missing", unavailable: "SSH alias is missing." },
    ] }, actions), COMMAND_IDS.connectHost);
    expect(command.enabled).toBe(false);
    expect(command.isEnabled?.({})).toBe(false);
    command.run();
    expect(actions.showConnectHost).not.toHaveBeenCalled();
    expect(actions.connectHost).not.toHaveBeenCalled();
  });

  it.each([true, false])("uses saved host availability instead of a stale method snapshot (connectable: %s)", (connectable) => {
    const { actions, context } = setup();
    const target: ConnectionTarget = {
      kind: "ssh", host_id: "build", destination: "build.example",
      ...(connectable ? { unavailable: "The preferred SSH alias is missing." } : {}),
    };
    const command = findCommand(buildTerminalCommands({
      ...context,
      targets: [target],
      connectableHostKeys: new Set(connectable ? ["host:build"] : []),
    }, actions), COMMAND_IDS.connectHost);
    expect(command.enabled).toBe(connectable);
    expect(command.isEnabled?.({ target_key: "host:build" })).toBe(connectable);
    command.run();
    command.run({ target_key: "host:build" });
    expect(actions.showConnectHost).toHaveBeenCalledTimes(connectable ? 1 : 0);
    expect(actions.connectHost).toHaveBeenCalledTimes(connectable ? 1 : 0);
    if (connectable) expect(actions.connectHost).toHaveBeenCalledWith(target);
  });

  it.each(["local", "host:missing", "host:unknown"])("does not connect an unusable explicit target %s or fall back to the picker", (target_key) => {
    const { actions, context } = setup();
    const command = findCommand(buildTerminalCommands({ ...context, targets: [
      { kind: "local" },
      { kind: "ssh", host_id: "available", destination: "available" },
      { kind: "ssh", host_id: "missing", destination: "missing", unavailable: "SSH alias is missing." },
    ] }, actions), COMMAND_IDS.connectHost);
    expect(command.enabled).toBe(true);
    expect(command.isEnabled?.({ target_key })).toBe(false);
    command.run({ target_key });
    expect(actions.connectHost).not.toHaveBeenCalled();
    expect(actions.showConnectHost).not.toHaveBeenCalled();
  });

  it.each(["confirmation", "restarting"])("blocks host picking and explicit connection during daemon restart %s", (phase) => {
    const { actions, context } = setup();
    const command = findCommand(buildTerminalCommands({
      ...context,
      targets: [{ kind: "ssh", host_id: "build", destination: "build" }],
      connectableHostKeys: new Set(["host:build"]),
      daemonRestartConfirmationPending: phase === "confirmation",
      restartingDaemon: phase === "restarting",
    }, actions), COMMAND_IDS.connectHost);
    expect(command.enabled).toBe(false);
    expect(command.isEnabled?.({ target_key: "host:build" })).toBe(false);
    command.run();
    command.run({ target_key: "host:build" });
    expect(actions.showConnectHost).not.toHaveBeenCalled();
    expect(actions.connectHost).not.toHaveBeenCalled();
  });

  it("opens the separate routed-host setup action", () => {
    const { actions, commands } = setup();
    findCommand(commands, COMMAND_IDS.addRoutedHost).run();
    expect(actions.showAddRoutedHost).toHaveBeenCalledOnce();
    expect(actions.showAddHost).not.toHaveBeenCalled();
  });

  it("opens new-shell quick input without refocusing the terminal", () => {
    const { actions, commands } = setup();
    const command = findCommand(commands, COMMAND_IDS.newShell);
    expect(command.focusTerminalAfterRun).toBe(false);
    expect(command.keybinding).toEqual({
      code: "KeyN",
      primary: true,
      shift: true,
    });
    command.run();
    expect(actions.showNewShell).toHaveBeenCalledOnce();
  });

  it("cycles through open tabs and wraps around", () => {
    const { tabs, actions, commands } = setup("first", "macos", "/work/rmux", [
      "first",
      "third",
    ]);

    findCommand(commands, COMMAND_IDS.nextTab).run();
    findCommand(commands, COMMAND_IDS.previousTab).run();

    expect(actions.selectSession).toHaveBeenNthCalledWith(1, tabs[1]);
    expect(actions.selectSession).toHaveBeenNthCalledWith(2, tabs[1]);
  });

  it("uses the existing confirmation flow for close", () => {
    const { sessions, actions, commands } = setup("second");

    const close = findCommand(commands, COMMAND_IDS.close);
    expect(close.enabled).toBe(true);
    expect(close.focusTerminalAfterRun).toBe(false);
    close.run();

    expect(actions.requestCloseSession).toHaveBeenCalledWith(sessions[1]);
    expect(actions.confirmCloseSession).not.toHaveBeenCalled();
  });

  it.each(["first", "second", null])(
    "confirms the pending session while %s is active",
    (activeSessionId) => {
      const { sessions, actions, commands } = setup(
        activeSessionId,
        "macos",
        "/work/rmux",
        ["first", "second", "third"],
        "attached",
        "second",
        "second",
      );

      const close = findCommand(commands, COMMAND_IDS.close);
      expect(close).toMatchObject({
        title: "Confirm Terminate Session",
        detail: "second",
        enabled: true,
      });

      close.run();
      expect(actions.confirmCloseSession).toHaveBeenCalledExactlyOnceWith(
        sessions[1],
      );
      expect(actions.requestCloseSession).not.toHaveBeenCalled();
    },
  );

  it("does not fall back to the active session when the pending session disappears", () => {
    const { actions, commands } = setup(
      "first",
      "macos",
      "/work/rmux",
      undefined,
      "attached",
      "first",
      "missing",
    );
    const close = findCommand(commands, COMMAND_IDS.close);
    expect(close.enabled).toBe(false);
    close.run();
    expect(actions.confirmCloseSession).not.toHaveBeenCalled();
    expect(actions.requestCloseSession).not.toHaveBeenCalled();
  });

  it.each(["closing", "disconnecting", "restart-confirmation", "restarting"])(
    "disables confirmation while its target is %s",
    (state) => {
      const { commands } = setup(
        "first",
        "macos",
        "/work/rmux",
        undefined,
        "attached",
        "first",
        "second",
        state === "restart-confirmation",
        state === "restarting",
        "/work/rmux",
        state === "closing" ? ["second"] : [],
        state === "disconnecting" ? "second" : null,
      );
      expect(findCommand(commands, COMMAND_IDS.close).enabled).toBe(false);
    },
  );

  it("does not let the active tab's in-flight operation disable a different pending close", () => {
    const { commands } = setup(
      "first",
      "macos",
      "/work/rmux",
      undefined,
      "attached",
      "first",
      "second",
      false,
      false,
      "/work/rmux",
      ["first"],
    );
    expect(findCommand(commands, COMMAND_IDS.close).enabled).toBe(true);
  });

  it("uses native macOS and terminal-safe cross-platform close shortcuts", () => {
    const macos = setup("first", "macos");
    const other = setup("first", "other");

    expect(findCommand(macos.commands, COMMAND_IDS.disconnect)).toMatchObject({
      keybinding: { code: "KeyW", primary: true, shift: false },
    });
    expect(findCommand(macos.commands, COMMAND_IDS.close)).toMatchObject({
      keybinding: { code: "KeyE", primary: true, shift: false },
    });
    expect(
      findCommand(other.commands, COMMAND_IDS.disconnect).keybinding,
    ).toEqual({
      code: "KeyW",
      primary: true,
      shift: true,
    });
    expect(findCommand(other.commands, COMMAND_IDS.close).keybinding).toEqual({
      code: "KeyE",
      primary: true,
      shift: true,
    });
  });

  it("opens a shell tab with the platform-specific shortcut", () => {
    const macos = setup("first", "macos");
    const other = setup("first", "other");

    const macosCommand = findCommand(macos.commands, COMMAND_IDS.newTab);
    const otherCommand = findCommand(other.commands, COMMAND_IDS.newTab);
    expect(macosCommand.keybinding).toEqual({
      code: "KeyT",
      primary: true,
      shift: false,
    });
    expect(otherCommand.keybinding).toEqual({
      code: "KeyT",
      primary: true,
      shift: true,
    });

    macosCommand.run();
    expect(macos.actions.openShellTab).toHaveBeenCalledOnce();
  });

  it("requires a shell-reported cwd before opening a tab", () => {
    const { commands } = setup("first", "macos", null);
    const command = findCommand(commands, COMMAND_IDS.newTab);

    expect(command.enabled).toBe(false);
    expect(command.disabledReason).toContain("working directory");
  });

  it("publishes one dynamic switch command per session", () => {
    const { sessions, commands } = setup("second");
    const switchCommands = commands.filter((command) =>
      command.id.startsWith("session.switch."),
    );

    expect(switchCommands).toHaveLength(3);
    expect(
      switchCommands.find(
        ({ id }) => id === sessionSwitchCommandId(sessions[1]),
      )?.enabled,
    ).toBe(false);
  });

  it("keeps a selected session retryable after its attachment fails", () => {
    const { sessions, commands } = setup(
      "second",
      "macos",
      "/work/rmux",
      ["first", "second"],
      "error",
      "second",
    );

    const retry = findCommand(commands, sessionSwitchCommandId(sessions[1]));
    expect(retry).toMatchObject({
      title: "Reconnect to second",
      detail: "Retry attachment",
      enabled: true,
    });
  });

  it("keeps a selected tab attachable when no attachment exists", () => {
    const { sessions, commands } = setup(
      "second",
      "macos",
      "/work/rmux",
      ["first", "second"],
      "idle",
      null,
    );

    expect(
      findCommand(commands, sessionSwitchCommandId(sessions[1])),
    ).toMatchObject({
      title: "Attach to second",
      detail: "Retry attachment",
      enabled: true,
    });
  });

  it("does not start another attachment for a selected connecting tab", () => {
    const { sessions, commands } = setup(
      "second",
      "macos",
      "/work/rmux",
      ["first", "second"],
      "connecting",
      "second",
    );

    const connecting = findCommand(
      commands,
      sessionSwitchCommandId(sessions[1]),
    );
    expect(connecting).toMatchObject({
      detail: "Attaching…",
      enabled: false,
    });
  });

  it("does not expose controls for an attachment behind another selected tab", () => {
    const { commands } = setup(
      "second",
      "macos",
      "/work/rmux",
      ["first", "second"],
      "attached",
      "first",
    );

    expect(findCommand(commands, COMMAND_IDS.toggleInput).enabled).toBe(false);
    expect(findCommand(commands, COMMAND_IDS.toggleResize).enabled).toBe(false);
    expect(findCommand(commands, COMMAND_IDS.reconnect).enabled).toBe(false);
  });

  it("does not expose disabled session actions without an active session", () => {
    const { commands } = setup(null);

    expect(findCommand(commands, COMMAND_IDS.disconnect).enabled).toBe(false);
    expect(findCommand(commands, COMMAND_IDS.close).enabled).toBe(false);
  });

  it("stages a destructive daemon restart without a shortcut", () => {
    const initial = setup();
    const restart = findCommand(initial.commands, COMMAND_IDS.restartDaemon);

    expect(restart).toMatchObject({
      category: "Daemon",
      title: "Restart rmuxd",
      keepPaletteOpen: false,
      focusTerminalAfterRun: false,
    });
    expect(restart.keybinding).toBeUndefined();
    restart.run();
    expect(initial.actions.requestDaemonRestart).toHaveBeenCalledOnce();

    const confirmation = setup(
      "first",
      "macos",
      "/work/rmux",
      ["first", "second", "third"],
      "attached",
      "first",
      null,
      true,
    );
    const confirm = findCommand(
      confirmation.commands,
      COMMAND_IDS.restartDaemon,
    );

    expect(confirm).toMatchObject({
      title: "Restart rmuxd",
      enabled: false,
      keepPaletteOpen: false,
    });
    expect(
      findCommand(confirmation.commands, COMMAND_IDS.newShell).enabled,
    ).toBe(false);
    expect(confirmation.actions.requestDaemonRestart).not.toHaveBeenCalled();
  });
});
