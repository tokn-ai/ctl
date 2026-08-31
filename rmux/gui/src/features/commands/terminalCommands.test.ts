import { describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "../../lib/types";
import {
  buildTerminalCommands,
  COMMAND_IDS,
} from "./terminalCommands";

function session(id: string): SessionSummary {
  return {
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
) {
  const sessions = [session("first"), session("second"), session("third")];
  const tabs = sessions.filter((candidate) =>
    tabIds.includes(candidate.session_id),
  );
  const actions = {
    showPalette: vi.fn(),
    showNewShellForm: vi.fn(),
    openShellTab: vi.fn(),
    refreshSessions: vi.fn(),
    selectSession: vi.fn(),
    disconnectSession: vi.fn(),
    requestCloseSession: vi.fn(),
    toggleInput: vi.fn(),
    toggleResizeWithWindow: vi.fn(),
    reconnect: vi.fn(),
    focusTerminal: vi.fn(),
  };
  const commands = buildTerminalCommands(
    {
      sessions,
      tabs,
      activeSessionId,
      phase: "attached",
      inputOwned: true,
      resizeWithWindow: false,
      listLoading: false,
      creating: false,
      createFormOpen: false,
      closingSessionIds: new Set(),
      disconnectingSessionId: null,
      terminalReady: true,
      currentWorkingDirectory,
      shortcutPlatform,
    },
    actions,
  );
  return { sessions, tabs, actions, commands };
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
  it("cycles through open tabs and wraps around", () => {
    const { tabs, actions, commands } = setup(
      "first",
      "macos",
      "/work/rmux",
      ["first", "third"],
    );

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
    const { commands } = setup("second");
    const switchCommands = commands.filter((command) =>
      command.id.startsWith("session.switch."),
    );

    expect(switchCommands).toHaveLength(3);
    expect(switchCommands.find(({ id }) => id.endsWith("second"))?.enabled).toBe(
      false,
    );
  });

  it("does not expose disabled session actions without an active session", () => {
    const { commands } = setup(null);

    expect(findCommand(commands, COMMAND_IDS.disconnect).enabled).toBe(false);
    expect(findCommand(commands, COMMAND_IDS.close).enabled).toBe(false);
  });
});
