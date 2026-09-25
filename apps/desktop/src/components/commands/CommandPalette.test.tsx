// @vitest-environment jsdom
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { AppCommand } from "../../features/commands/types";
import { CommandPalette } from "./CommandPalette";

afterEach(cleanup);

describe("CommandPalette", () => {
  it("activates the focused close button without executing a search result", async () => {
    const dismiss = vi.fn(),
      execute = vi.fn();
    render(
      <CommandPalette
        commands={[
          {
            id: "session.close",
            category: "Session",
            title: "Close Active Session",
            enabled: true,
            run: vi.fn(),
          },
        ]}
        platform="macos"
        onDismiss={dismiss}
        onExecute={execute}
      />,
    );
    screen.getByRole("button", { name: "Close command palette" }).focus();
    await userEvent.setup().keyboard("{Enter}");
    expect(dismiss).toHaveBeenCalledOnce();
    expect(execute).not.toHaveBeenCalled();
  });

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
