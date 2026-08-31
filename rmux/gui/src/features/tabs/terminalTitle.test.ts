import { describe, expect, it } from "vitest";
import type { ShellStateSummary } from "../../lib/types";
import {
  compactTerminalTitle,
  compactTerminalTitleParts,
  formatTerminalTitle,
} from "./terminalTitle";

function shellState(
  overrides: Partial<ShellStateSummary> = {},
): ShellStateSummary {
  return {
    shell_type: "zsh",
    cwd: "/Users/clouds/Projects/Tools/ctl",
    running_command: null,
    prompt_phase: "at_prompt",
    tui_hint: "inline",
    revision: "1",
    observed_sequence: "1",
    ...overrides,
  };
}

describe("formatTerminalTitle", () => {
  it("joins the observed path and short running command", () => {
    expect(
      formatTerminalTitle(
        { name: "session-1" },
        shellState({
          running_command: "cargo test -p rmux-gui",
          prompt_phase: "running",
        }),
      ),
    ).toEqual({
      path: "/Users/clouds/Projects/Tools/ctl",
      command: "cargo test -p rmux-gui",
      text: "/Users/clouds/Projects/Tools/ctl — cargo test -p rmux-gui",
    });
  });

  it("uses the shell when the session is idle", () => {
    expect(
      formatTerminalTitle({ name: "session-1" }, shellState()),
    ).toEqual({
      path: "/Users/clouds/Projects/Tools/ctl",
      command: "zsh",
      text: "/Users/clouds/Projects/Tools/ctl — zsh",
    });
  });

  it("does not show stale command text after the shell returns to its prompt", () => {
    expect(
      formatTerminalTitle(
        { name: "session-1" },
        shellState({ running_command: "cargo test" }),
      ),
    ).toMatchObject({ command: "zsh" });
  });

  it("falls back without inventing a path or command", () => {
    expect(formatTerminalTitle({ name: "session-1" }, null)).toEqual({
      path: "session-1",
      command: null,
      text: "session-1",
    });
    expect(
      formatTerminalTitle(
        { name: "session-1" },
        shellState({ cwd: null, shell_type: "unknown" }),
      ),
    ).toEqual({
      path: "session-1",
      command: null,
      text: "session-1",
    });
  });
});

describe("compactTerminalTitle", () => {
  it("leaves a short title unchanged", () => {
    const title = "/Users/clouds/Projects/Tools/ctl — zsh";

    expect(compactTerminalTitle(title, title.length)).toBe(title);
  });

  it("omits the shared prefix and preserves the distinguishing tail", () => {
    expect(
      compactTerminalTitle(
        "/Users/clouds/Projects/Tools/ctl/rmux/gui/src/features/tabs — cargo test",
        31,
      ),
    ).toBe("…src/features/tabs — cargo test");
  });

  it("counts Unicode characters without splitting a surrogate pair", () => {
    expect(compactTerminalTitle("shared-prefix/terminal-🦀", 11)).toBe(
      "…terminal-🦀",
    );
  });
});

describe("compactTerminalTitleParts", () => {
  it("preserves both the path tail and command tail when both are long", () => {
    const title = formatTerminalTitle(
      { name: "session-1" },
      shellState({
        cwd: "/Users/clouds/Projects/Tools/ctl/rmux/gui/src/features/tabs",
        running_command:
          "pnpm exec vitest run terminalTitle --watch=false --reporter=verbose",
        prompt_phase: "running",
      }),
    );

    const compact = compactTerminalTitleParts(title, 40);
    const [path, command] = compact.split(" — ");

    expect(path).toMatch(/^…/);
    expect(path.endsWith("features/tabs")).toBe(true);
    expect(command).toMatch(/^…/);
    expect(command.endsWith("reporter=verbose")).toBe(true);
    expect(Array.from(compact)).toHaveLength(40);
  });
});
