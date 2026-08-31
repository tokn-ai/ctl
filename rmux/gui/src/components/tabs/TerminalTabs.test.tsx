import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "../../lib/types";
import { TerminalTabs } from "./TerminalTabs";

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

describe("TerminalTabs", () => {
  it("marks the active tab and explains that tab close preserves the session", () => {
    const markup = renderToStaticMarkup(
      <TerminalTabs
        tabs={[session("first"), session("second")]}
        activeSessionId="second"
        canCreate
        onSelect={vi.fn()}
        onClose={vi.fn()}
        onCreate={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Terminal tabs"');
    expect(markup).toContain('aria-selected="true"');
    expect(markup).toContain('title="Close tab; keep session running"');
    expect(markup).toContain('aria-label="New tab in current folder"');
  });
});
