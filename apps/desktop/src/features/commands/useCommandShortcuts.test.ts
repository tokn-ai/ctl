// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AppCommand } from "./types";
import { isWebviewKeybinding, useCommandShortcuts } from "./useCommandShortcuts";

afterEach(cleanup);

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
  it("leaves keys in local preview controls out of terminal command dispatch", () => {
    const execute = vi.fn();
    renderHook(() => useCommandShortcuts([command()], "other", execute));
    const preview = document.createElement("section");
    preview.dataset.localShortcuts = "";
    const content = document.createElement("div");
    preview.append(content);
    document.body.append(preview);
    const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, code: "KeyW", ctrlKey: true });
    content.dispatchEvent(event);
    expect(execute).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
    preview.remove();
    window.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, code: "KeyW", ctrlKey: true }));
    expect(execute).toHaveBeenCalledOnce();
  });

  it("leaves native macOS accelerators to the application menu", () => {
    const nativeCommand = command({ macosNativeKeybinding: true });

    expect(isWebviewKeybinding(nativeCommand, "macos")).toBe(false);
    expect(isWebviewKeybinding(nativeCommand, "other")).toBe(true);
    expect(isWebviewKeybinding(command(), "macos")).toBe(true);
  });
});
