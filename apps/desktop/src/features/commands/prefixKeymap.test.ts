import { describe, expect, it } from "vitest";
import { resolveKeymap } from "./keymap";
import { prefixStroke, resolvePrefix } from "./prefixKeymap";
import type { KeybindingsDocument } from "../../lib/types";
const document: KeybindingsDocument = { schema_version: 1, overrides: [] };
describe("terminal prefix settings", () => {
  it("keeps legacy documents and generates literal Ctrl bytes on either platform", () => {
    for (const platform of ["macos", "other"] as const) {
      expect(resolvePrefix(document, resolveKeymap(document, platform), platform).stroke?.bytes).toEqual(new Uint8Array([2]));
    }
    expect(prefixStroke("Ctrl+A").bytes).toEqual(new Uint8Array([1]));
    expect(prefixStroke("Alt+B").bytes).toEqual(new Uint8Array([27, 98]));
  });
  it("rejects ambiguous actions and prefix conflicts with direct commands", () => {
    expect(() => resolveKeymap({ ...document, prefix: { key: "Ctrl+B", bindings: [{ command_id: "pane.zoom", key: "V" }] } }, "other")).toThrow(/already assigned/);
    expect(() => resolveKeymap({ ...document, prefix: { key: "Ctrl+B", bindings: [] }, overrides: [{ command_id: "terminal.focus", keybinding: { primary: true, code: "KeyB" } }] }, "other")).toThrow(/prefix is already assigned/);
    expect(() => resolveKeymap({ ...document, prefix: { key: "B", bindings: [] } }, "macos")).toThrow(/Ctrl\+letter/);
  });
  it("preserves a legacy direct shortcut that occupies the default prefix", () => {
    const legacy: KeybindingsDocument = { ...document, overrides: [{ command_id: "terminal.focus", keybinding: { primary: true, code: "KeyB" } }] };
    const direct = resolveKeymap(legacy, "other");
    expect(direct.get("terminal.focus")?.code).toBe("KeyB");
    expect(resolvePrefix(legacy, direct, "other").stroke).toBeNull();
  });
  it("supports disabling the prefix and remapping or unbinding actions", () => {
    const result = resolvePrefix({ ...document, prefix: { key: null, bindings: [{ command_id: "pane.zoom", key: "Q" }, { command_id: "pane.move", key: null }] } }, new Map(), "macos");
    expect(result.stroke).toBeNull();
    expect(result.bindings.get("pane.zoom")).toBe("Q");
    expect(result.bindings.has("pane.move")).toBe(false);
  });
});
