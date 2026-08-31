import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionSummary, ShellStateSummary } from "../../lib/types";
import { SessionSidebar } from "./SessionSidebar";

const session: SessionSummary = {
  session_id: "first",
  name: "first",
  status: "running",
  terminal_size: {
    columns: 80,
    rows: 24,
    pixel_width: null,
    pixel_height: null,
  },
  next_sequence: "0",
};

const shellState: ShellStateSummary = {
  shell_type: "zsh",
  cwd: "/Users/clouds/Projects/Tools/ctl/rmux/gui",
  running_command: "cargo test -p rmux-gui",
  prompt_phase: "running",
  tui_hint: "inline",
  revision: "1",
  observed_sequence: "1",
};

describe("SessionSidebar", () => {
  it("focuses Close when a destructive confirmation opens", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        sessions={[session]}
        shellStates={new Map()}
        selectedSessionId="first"
        disconnectableSessionId="first"
        loading={false}
        error={null}
        creating={false}
        createFormOpen={false}
        pendingCloseSessionId="first"
        closingSessionIds={new Set()}
        disconnectingSessionId={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onCreate={vi.fn(async () => true)}
        onCreateFormOpenChange={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onCancelClose={vi.fn()}
        onConfirmClose={vi.fn()}
      />,
    );

    expect(markup).toMatch(/class="session-confirm-close"[^>]*autofocus/);
    expect(markup).not.toMatch(/class="session-confirm-cancel"[^>]*autofocus/);
  });

  it("uses the observed terminal title as the primary label", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        sessions={[session]}
        shellStates={new Map([[session.session_id, shellState]])}
        selectedSessionId={null}
        disconnectableSessionId={null}
        loading={false}
        error={null}
        creating={false}
        createFormOpen={false}
        pendingCloseSessionId={null}
        closingSessionIds={new Set()}
        disconnectingSessionId={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onCreate={vi.fn(async () => true)}
        onCreateFormOpenChange={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onCancelClose={vi.fn()}
        onConfirmClose={vi.fn()}
      />,
    );

    const fullTitle = "/Users/clouds/Projects/Tools/ctl/rmux/gui — cargo test -p rmux-gui";
    expect(markup).toContain(`title="${fullTitle}"`);
    expect(markup).toContain("<strong>…rmux/gui — …mux-gui</strong>");
    expect(markup).toContain(
      `<small>${session.name}<span aria-hidden="true"> · </span>`,
    );
    expect(markup).not.toContain(`<strong>${session.name}</strong>`);
  });

  it("uses a neutral primary label until shell state is observed", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        sessions={[session]}
        shellStates={new Map()}
        selectedSessionId={null}
        disconnectableSessionId={null}
        loading={false}
        error={null}
        creating={false}
        createFormOpen={false}
        pendingCloseSessionId={null}
        closingSessionIds={new Set()}
        disconnectingSessionId={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onCreate={vi.fn(async () => true)}
        onCreateFormOpenChange={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onCancelClose={vi.fn()}
        onConfirmClose={vi.fn()}
      />,
    );

    expect(markup).toContain("<strong>Shell</strong>");
    expect(markup).toContain(session.name);
  });
});
