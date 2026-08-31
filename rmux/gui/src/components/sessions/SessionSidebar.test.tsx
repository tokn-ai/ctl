import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "../../lib/types";
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

describe("SessionSidebar", () => {
  it("focuses Close when a destructive confirmation opens", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        sessions={[session]}
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
});
