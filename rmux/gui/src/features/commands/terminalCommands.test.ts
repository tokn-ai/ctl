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

function setup(activeSessionId: string | null = "first") {
  const sessions = [session("first"), session("second"), session("third")];
  const actions = {
    showPalette: vi.fn(),
    showNewShellForm: vi.fn(),
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
    },
    actions,
  );
  return { sessions, actions, commands };
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
  it("cycles through sessions and wraps around", () => {
    const { sessions, actions, commands } = setup("first");

    findCommand(commands, COMMAND_IDS.nextSession).run();
    findCommand(commands, COMMAND_IDS.previousSession).run();

    expect(actions.selectSession).toHaveBeenNthCalledWith(1, sessions[1]);
    expect(actions.selectSession).toHaveBeenNthCalledWith(2, sessions[2]);
  });

  it("uses the existing confirmation flow for close", () => {
    const { sessions, actions, commands } = setup("second");

    const close = findCommand(commands, COMMAND_IDS.close);
    expect(close.enabled).toBe(true);
    expect(close.focusTerminalAfterRun).toBe(false);
    close.run();

    expect(actions.requestCloseSession).toHaveBeenCalledWith(sessions[1]);
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
