import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { AppCommand } from "../../features/commands/types";
import { CommandPalette } from "./CommandPalette";

describe("CommandPalette", () => {
  it("renders discoverable commands, disabled reasons, and shortcut labels", () => {
    const commands: AppCommand[] = [
      {
        id: "session.new_shell",
        category: "Session",
        title: "New Shell",
        enabled: true,
        keybinding: { code: "KeyN", primary: true, shift: true },
        run: vi.fn(),
      },
      {
        id: "session.close",
        category: "Session",
        title: "Close Active Session",
        enabled: false,
        disabledReason: "No session is active.",
        run: vi.fn(),
      },
      {
        id: "internal",
        category: "View",
        title: "Hidden Command",
        enabled: true,
        visibleInPalette: false,
        run: vi.fn(),
      },
    ];

    const markup = renderToStaticMarkup(
      <CommandPalette
        commands={commands}
        platform="macos"
        onDismiss={vi.fn()}
        onExecute={vi.fn()}
      />,
    );

    expect(markup).toContain("New Shell");
    expect(markup).toContain("⌘⇧N");
    expect(markup).toContain("No session is active.");
    expect(markup).not.toContain("Hidden Command");
    expect(markup).toContain('role="dialog"');
    expect(markup).toContain('role="listbox"');
  });
});
