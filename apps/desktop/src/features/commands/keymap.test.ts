import { describe, expect, it } from "vitest";
import { COMMAND_IDS, QUICK_INPUT_IDS } from "./commandIds";
import {
  defaultKeybindings,
  editableKeybinding,
  EMPTY_KEYBINDINGS,
  parseKeybinding,
  resolveKeymap,
} from "./keymap";
import type { KeybindingsDocument } from "../../lib/types";

function document(command_id: string, text: string): KeybindingsDocument {
  return {
    schema_version: 1,
    overrides: [{ command_id, keybinding: parseKeybinding(text) }],
  };
}

describe("configurable command keymap", () => {
  it("preserves platform defaults and supports overrides, unbinding, and reset", () => {
    expect(defaultKeybindings("macos").get(COMMAND_IDS.close)?.shift).toBe(
      false,
    );
    expect(defaultKeybindings("other").get(COMMAND_IDS.close)?.shift).toBe(
      true,
    );
    expect(
      resolveKeymap(
        document(COMMAND_IDS.close, "Primary+Shift+Y"),
        "macos",
      ).get(COMMAND_IDS.close),
    ).toEqual(parseKeybinding("Primary+Shift+Y"));
    expect(
      resolveKeymap(document(COMMAND_IDS.close, ""), "macos").has(
        COMMAND_IDS.close,
      ),
    ).toBe(false);
    expect(
      resolveKeymap(EMPTY_KEYBINDINGS, "macos").get(COMMAND_IDS.close)?.code,
    ).toBe("KeyE");
  });

  it.each([
    "Primary+Shift+E",
    "Alt+F12",
    "Escape",
    "Primary+1",
    "Alt+ArrowUp",
    "Alt+Minus",
    "Primary+\\",
    "Primary+=",
    "Alt+PageDown",
  ])("round-trips %s through the settings editor", (text) => {
    const binding = parseKeybinding(text)!;
    expect(parseKeybinding(editableKeybinding(binding))).toEqual(binding);
  });

  it.each([
    "Primary+Ctrl+E",
    "Alt+Option+E",
    "Shift+Shift+E",
    "Super+E",
    "Primary+",
    "F13",
    "Tab",
  ])("rejects unsupported or ambiguous input %s", (text) => {
    expect(() => parseKeybinding(text)).toThrow();
  });

  it("rejects conflicts, unknown commands, duplicate overrides, and unsafe terminal keys", () => {
    expect(() =>
      resolveKeymap(document(COMMAND_IDS.close, "Primary+Shift+N"), "other"),
    ).toThrow(/already assigned/);
    expect(() =>
      resolveKeymap(
        document(QUICK_INPUT_IDS.cancel, "Primary+Shift+N"),
        "other",
      ),
    ).toThrow(/already assigned/);
    expect(() => resolveKeymap(document("unknown", "Alt+F2"), "other")).toThrow(
      /Unknown/,
    );
    const duplicate = document(COMMAND_IDS.close, "Alt+F2");
    duplicate.overrides.push(...duplicate.overrides);
    expect(() => resolveKeymap(duplicate, "other")).toThrow(/duplicate/);
    expect(() =>
      resolveKeymap(document(COMMAND_IDS.close, "E"), "other"),
    ).toThrow(/terminal typing/);
    expect(() =>
      resolveKeymap(document(COMMAND_IDS.close, "Primary+Q"), "macos"),
    ).toThrow(/reserved/);
  });

  it("can assign previously unbound app and dialog commands without changing defaults", () => {
    const settings = document(COMMAND_IDS.refreshSessions, "Alt+F2");
    const original = JSON.stringify(settings);
    expect(
      resolveKeymap(settings, "other").has(COMMAND_IDS.refreshSessions),
    ).toBe(true);
    expect(
      resolveKeymap(
        document(QUICK_INPUT_IDS.accept, "Primary+Enter"),
        "other",
      ).has(QUICK_INPUT_IDS.accept),
    ).toBe(true);
    expect(JSON.stringify(settings)).toBe(original);
    expect(defaultKeybindings("other").has(COMMAND_IDS.refreshSessions)).toBe(
      false,
    );
  });
});
