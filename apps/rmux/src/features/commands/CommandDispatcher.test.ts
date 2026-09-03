import { describe, expect, it, vi } from "vitest";
import { CommandDispatcher } from "./CommandDispatcher";
import type { AppCommand } from "./types";

function command(id: string, overrides: Partial<AppCommand> = {}): AppCommand {
  return {
    id,
    title: id,
    category: "Test",
    enabled: true,
    run: vi.fn(),
    ...overrides,
  };
}

describe("shared command dispatcher", () => {
  it("enforces readiness, availability, and fresh descriptors for every caller", () => {
    const dispatcher = new CommandDispatcher();
    const original = command("close");
    dispatcher.update([original], false, vi.fn());
    expect(dispatcher.execute("close")).toBe(false);
    dispatcher.update([original], true, vi.fn());
    expect(dispatcher.execute("unknown")).toBe(false);
    dispatcher.update([command("close", { enabled: false })], true, vi.fn());
    expect(dispatcher.execute(original.id)).toBe(false);
    expect(original.run).not.toHaveBeenCalled();
  });

  it("checks availability against explicit arguments, not the active tab", () => {
    const action = command("close", {
      enabled: false,
      isEnabled: (args) => args.session_key === "second",
    });
    const dispatcher = new CommandDispatcher();
    dispatcher.update([action], true, vi.fn());
    expect(dispatcher.execute("close")).toBe(false);
    expect(dispatcher.execute("close", { session_key: "missing" })).toBe(false);
    expect(dispatcher.execute("close", { session_key: "second" })).toBe(true);
    expect(action.run).toHaveBeenCalledExactlyOnceWith({
      session_key: "second",
    });
  });

  it("isolates dialogs and resolves close only through the current confirmation", () => {
    const app = command("session.close");
    const accept = command("quick_input.accept");
    const dispatcher = new CommandDispatcher();
    dispatcher.update([app, command("host.remove")], true, vi.fn());
    const token = Symbol();
    dispatcher.setScope(token, {
      commands: [accept],
      redirects: { "session.close": "quick_input.accept" },
    });
    expect(dispatcher.execute("host.remove")).toBe(false);
    expect(dispatcher.execute("session.close")).toBe(true);
    expect(app.run).not.toHaveBeenCalled();
    expect(accept.run).toHaveBeenCalledOnce();
    dispatcher.setScope(token, { commands: [accept] });
    expect(dispatcher.execute("session.close")).toBe(false);
    dispatcher.removeScope(token);
    expect(dispatcher.execute("quick_input.accept")).toBe(false);
    expect(dispatcher.execute("session.close")).toBe(true);
    expect(app.run).toHaveBeenCalledOnce();
  });

  it("lets the palette dispatch app commands but a nested dialog takes precedence", () => {
    const dispatcher = new CommandDispatcher();
    dispatcher.update([command("new")], true, vi.fn());
    const palette = Symbol(),
      dialog = Symbol();
    dispatcher.setScope(palette, { commands: [], allow_app_commands: true });
    expect(dispatcher.canExecute("new")).toBe(true);
    dispatcher.setScope(dialog, { commands: [] });
    dispatcher.setScope(palette, { commands: [], allow_app_commands: true });
    expect(dispatcher.canExecute("new")).toBe(false);
    dispatcher.removeScope(dialog);
    expect(dispatcher.canExecute("new")).toBe(true);
  });

  it("keeps async actions single-flight across rerenders and reports failures", async () => {
    let reject!: (error: Error) => void;
    const error = new Error("failed");
    const onError = vi.fn();
    const action = command("save", {
      run: () =>
        new Promise<void>((_resolve, fail) => {
          reject = fail;
        }),
    });
    const dispatcher = new CommandDispatcher();
    dispatcher.update([action], true, onError);
    expect(dispatcher.execute("save")).toBe(true);
    dispatcher.update([{ ...action }], true, onError);
    expect(dispatcher.execute("save")).toBe(false);
    reject(error);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(onError).toHaveBeenCalledExactlyOnceWith(error);
    expect(dispatcher.canExecute("save")).toBe(true);
  });
});
