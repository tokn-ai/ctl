import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionSummary, ShellStateSummary, ViewLayout } from "../../lib/types";
import { sessionKey } from "../../features/targets/targets";
import { hostIdColor } from "../../features/workspace/hostColor";
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

    expect(markup).toContain('aria-label="Workspace tabs"');
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

  it.each([undefined, { local: "#e79acb", "build-server": "#91b7f2" }])("matches terminal and task accents by host ID with preferences %j", (host_colors) => {
    const local = session("local-shell");
    const remote: SessionSummary = {
      ...session("remote-shell"),
      target: {
        kind: "ssh", host_id: "build-server", host_name: "Build", destination: "100.64.0.2",
      },
    };
    const markup = renderToStaticMarkup(
      <TerminalTabs
        tabs={[local, remote]}
        host_colors={host_colors}
        extra_tabs={[{
          tab_key: "task:compile", title: "Compile", host: "Build", host_id: "build-server", status: "running",
        }]}
        shellStates={new Map()}
        activeSessionKey={sessionKey(remote)}
        canCreate
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
      />,
    );

    const accents = [...markup.matchAll(/style="--host-color:([^"]+)"/g)].map((match) => match[1]);
    expect(accents).toEqual(["#ffffff", host_colors?.["build-server"] ?? hostIdColor("build-server"), host_colors?.["build-server"] ?? hostIdColor("build-server")]);
    expect(markup).toContain('aria-label="remote-shell on Build"');
    expect(markup).toContain('title="remote-shell · Build"');
    expect(markup).toContain('aria-label="Compile on Build"');
    expect(markup).toContain('class="task-state-dot running"');
  });

  it("uses shell icons for single sessions and layout icons for split sessions", () => {
    const single = session("single");
    const split = session("split");
    const layout: ViewLayout = {
      kind: "split",
      axis: "horizontal",
      children: [
        { kind: "terminal", terminal_id: "left" },
        { kind: "terminal", terminal_id: "right" },
      ],
    };
    const markup = renderToStaticMarkup(
      <TerminalTabs
        tabs={[single, split]}
        shellStates={new Map([
          [sessionKey(single), { ...shellState(), shell_type: "fish" }],
          [sessionKey(split), shellState()],
        ])}
        session_layouts={new Map([[sessionKey(split), layout]])}
        activeSessionKey={sessionKey(single)}
        canCreate
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Fish session"');
    expect(markup).toContain('aria-label="2-pane session layout"');
    expect(markup).toContain('aria-label="/workspace — cargo test on local" aria-description="Fish session"');
    expect(markup).toContain('aria-label="/workspace — cargo test on local" aria-description="2-pane session layout"');
    expect(markup).not.toContain('aria-label="Zsh session"');
  });
});
