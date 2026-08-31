import { describe, expect, it } from "vitest";
import {
  formatKeybinding,
  matchesKeybinding,
} from "./keybindings";
import type { Keybinding } from "./types";

const PALETTE: Keybinding = {
  code: "KeyP",
  primary: true,
  shift: true,
};

function event(overrides: Partial<KeyboardEvent> = {}): KeyboardEvent {
  return {
    code: "KeyP",
    ctrlKey: false,
    metaKey: false,
    shiftKey: true,
    altKey: false,
    ...overrides,
  } as KeyboardEvent;
}

describe("command keybindings", () => {
  it("uses Command as the primary modifier on macOS", () => {
    expect(
      matchesKeybinding(event({ metaKey: true }), PALETTE, "macos"),
    ).toBe(true);
    expect(
      matchesKeybinding(event({ ctrlKey: true }), PALETTE, "macos"),
    ).toBe(false);
    expect(formatKeybinding(PALETTE, "macos")).toBe("⌘⇧P");
  });

  it("uses Control as the primary modifier elsewhere", () => {
    expect(
      matchesKeybinding(event({ ctrlKey: true }), PALETTE, "other"),
    ).toBe(true);
    expect(
      matchesKeybinding(event({ metaKey: true }), PALETTE, "other"),
    ).toBe(false);
    expect(formatKeybinding(PALETTE, "other")).toBe("Ctrl+Shift+P");
  });

  it("requires an exact modifier match", () => {
    expect(
      matchesKeybinding(
        event({ ctrlKey: true, altKey: true }),
        PALETTE,
        "other",
      ),
    ).toBe(false);
    expect(
      matchesKeybinding(
        event({ ctrlKey: true, shiftKey: false }),
        PALETTE,
        "other",
      ),
    ).toBe(false);
  });

  it("formats punctuation keys", () => {
    expect(
      formatKeybinding(
        { code: "BracketRight", primary: true, shift: true },
        "other",
      ),
    ).toBe("Ctrl+Shift+]");
  });
});
