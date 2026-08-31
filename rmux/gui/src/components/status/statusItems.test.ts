import { describe, expect, it } from "vitest";
import type { AttachmentViewState } from "../../lib/types";
import { createStatusItems } from "./statusItems";

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

describe("createStatusItems", () => {
  it("hides ownership noise, unknown shell state, and raw sequence numbers", () => {
    expect(createStatusItems(state()).map((item) => item.label)).toEqual([
      "107×24",
    ]);
  });

  it("shows concise useful terminal state", () => {
    const current = state({
      input_lease: { held: true, owned_by_client: false },
      shell_state: {
        shell_type: "zsh",
        cwd: "/Users/me/project",
        prompt_phase: "editing",
        tui_hint: "alternate_screen",
        revision: "2",
        observed_sequence: "58006",
      },
    });

    expect(createStatusItems(current).map((item) => item.label)).toEqual([
      "view only",
      "107×24",
      "zsh",
      "editing",
      "TUI",
      "/Users/me/project",
    ]);
  });

  it("shows view-only mode after this client releases input", () => {
    const current = state({
      input_lease: { held: false, owned_by_client: false },
    });

    expect(createStatusItems(current).map((item) => item.label)).toEqual([
      "view only",
      "107×24",
    ]);
  });
});
