import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionSummary, ShellStateSummary } from "../../lib/types";
import { sessionKey } from "../../features/targets/targets";
import { TerminalTabs } from "./TerminalTabs";

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

function shellState(): ShellStateSummary {
  return {
    shell_type: "zsh",
    cwd: "/workspace",
    running_command: "cargo test",
    prompt_phase: "running",
    tui_hint: "inline",
    revision: "1",
    observed_sequence: "1",
  };
}

describe("TerminalTabs", () => {
  it("marks the active tab and explains that tab close preserves the session", () => {
    const first = session("first");
    const second = session("second");
    const markup = renderToStaticMarkup(
      <TerminalTabs
        tabs={[first, second]}
        shellStates={new Map([[sessionKey(second), shellState()]])}
        activeSessionKey={sessionKey(second)}
        canCreate
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Terminal tabs"');
    expect(markup).toContain('aria-selected="true"');
    expect(markup).toContain('aria-label="/workspace — cargo test on local"');
    expect(markup).toContain(
      'class="terminal-tab-path"><bdi dir="ltr">/workspace</bdi>',
    );
    expect(markup).toContain('class="terminal-tab-command">cargo test');
    expect(markup).toContain('title="Close tab; keep session running"');
    expect(markup).toContain('aria-label="New tab in current folder"');
  });

  it("keeps a home-relative path in logical filesystem order", () => {
    const first = session("first");
    const homeRelativeState: ShellStateSummary = {
      ...shellState(),
      cwd: "/Users/clouds/Projects/Agents",
      cwd_display: "~/Projects/Agents",
    };
    const markup = renderToStaticMarkup(
      <TerminalTabs
        tabs={[first]}
        shellStates={new Map([[sessionKey(first), homeRelativeState]])}
        activeSessionKey={sessionKey(first)}
        canCreate
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
      />,
    );

    expect(markup).toContain(
      'class="terminal-tab-path"><bdi dir="ltr">~/Projects/Agents</bdi>',
    );
    expect(markup).not.toContain("Projects/Agents/~");
  });
});
