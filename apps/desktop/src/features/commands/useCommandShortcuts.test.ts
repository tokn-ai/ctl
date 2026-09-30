import { describe, expect, it, vi } from "vitest";
import type { AppCommand } from "./types";
import { isWebviewKeybinding } from "./useCommandShortcuts";

function command(overrides: Partial<AppCommand> = {}): AppCommand {
  return {
    id: "test",
    category: "Test",
    title: "Test command",
    keybinding: { code: "KeyW", primary: true },
    enabled: true,
    run: vi.fn(),
    ...overrides,
  };
}

describe("webview command shortcuts", () => {
  it("leaves native macOS accelerators to the application menu", () => {
    const nativeCommand = command({ macosNativeKeybinding: true });

    expect(isWebviewKeybinding(nativeCommand, "macos")).toBe(false);
    expect(isWebviewKeybinding(nativeCommand, "other")).toBe(true);
    expect(isWebviewKeybinding(command(), "macos")).toBe(true);
  });
});
