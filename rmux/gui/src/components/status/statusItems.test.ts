import { describe, expect, it } from "vitest";
import type { AttachmentViewState } from "../../lib/types";
import { createStatusGroups } from "./statusItems";

function state(overrides: Partial<AttachmentViewState> = {}): AttachmentViewState {
  return {
    phase: "attached",
    attachment_id: "attachment",
    session: {
      session_id: "session",
      name: "shell",
      status: "running",
      terminal_size: {
        columns: 107,
        rows: 24,
        pixel_width: null,
        pixel_height: null,
      },
      next_sequence: "58007",
    },
    input_lease: { held: true, owned_by_client: true },
    layout_lease: { held: false, owned_by_client: false },
    shell_state: {
      shell_type: "unknown",
      cwd: null,
      running_command: null,
      prompt_phase: "unknown",
      tui_hint: "unknown",
      revision: "1",
      observed_sequence: "58006",
    },
    applied_sequence: "58006",
    reconnect_sequence: null,
    history_gap: false,
    terminal_size_mismatch: false,
    resize_with_window: false,
    message: null,
    ...overrides,
  };
}

function labels(items: ReturnType<typeof createStatusGroups>) {
  return {
    context: items.context.map((entry) => entry.label),
    indicators: items.indicators.map((entry) => entry.label),
  };
}

describe("createStatusGroups", () => {
  it("shows control and geometry state without raw sequence numbers", () => {
    expect(labels(createStatusGroups(state()))).toEqual({
      context: [],
      indicators: ["INPUT", "FIXED", "107×24"],
    });
  });

  it("orders shell context separately from live terminal indicators", () => {
    const current = state({
      input_lease: { held: true, owned_by_client: false },
      layout_lease: { held: true, owned_by_client: false },
      shell_state: {
        shell_type: "zsh",
        cwd: "/Users/me/project",
        running_command: null,
        prompt_phase: "editing",
        tui_hint: "alternate_screen",
        revision: "2",
        observed_sequence: "58006",
      },
    });

    expect(labels(createStatusGroups(current))).toEqual({
      context: ["zsh", "/Users/me/project"],
      indicators: ["EDITING", "TUI", "VIEW", "OTHER SIZE", "107×24"],
    });
  });

  it("distinguishes active and pending window sizing", () => {
    const active = state({
      layout_lease: { held: true, owned_by_client: true },
      resize_with_window: true,
    });
    const pending = state({ resize_with_window: true });

    expect(labels(createStatusGroups(active)).indicators).toContain("WINDOW");
    expect(labels(createStatusGroups(pending)).indicators).toContain("STARTING SIZE");
  });

  it("prioritizes recovery and connection warnings while detached", () => {
    const current = state({
      phase: "reconnecting",
      history_gap: true,
      shell_state: {
        shell_type: "bash",
        cwd: "/work/rmux",
        running_command: "cargo test",
        prompt_phase: "running",
        tui_hint: "inline",
        revision: "3",
        observed_sequence: "58006",
      },
    });

    expect(labels(createStatusGroups(current))).toEqual({
      context: ["bash", "/work/rmux"],
      indicators: ["RUNNING", "107×24", "RECONNECTING", "HISTORY GAP"],
    });
  });

  it("explains why a view-only attachment cannot send input", () => {
    const unclaimed = createStatusGroups(state({
      input_lease: { held: false, owned_by_client: false },
    }));
    const ownedElsewhere = createStatusGroups(state({
      input_lease: { held: true, owned_by_client: false },
    }));

    expect(unclaimed.indicators.find((entry) => entry.key === "input")?.title)
      .toContain("does not own");
    expect(ownedElsewhere.indicators.find((entry) => entry.key === "input")?.title)
      .toContain("another attachment");
  });
});
