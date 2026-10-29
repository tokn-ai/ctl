// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CommandBindings } from "./CommandBindings";
import { CommandProvider } from "./CommandContext";
import { CommandDispatcher } from "./CommandDispatcher";
import { COMMAND_IDS } from "./commandIds";
import { defaultKeybindings } from "./keymap";
import { QuickInput } from "../../components/commands/QuickInput";
import { NotificationProvider } from "../notifications/NotificationContext";
import { NotificationStore } from "../notifications/NotificationStore";

const api = vi.hoisted(() => ({
  sync: vi.fn(),
  listener: null as ((event: { payload: string }) => void) | null,
}));
vi.mock("../../lib/tauri", () => ({ syncCommandMenu: api.sync }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, listener: typeof api.listener) => {
    api.listener = listener;
    return () => {
      api.listener = null;
    };
  }),
}));

beforeEach(() => {
  vi.clearAllMocks();
  api.sync.mockResolvedValue({ rejected: [] });
  vi.stubGlobal("__TAURI_INTERNALS__", {});
  vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("native and webview command adapters", () => {
  it("reports rejected commands while healthy native shortcuts work once, then clears the warning source on recovery", async () => {
    const dispatcher = new CommandDispatcher();
    const notifications = new NotificationStore();
    const report = vi.spyOn(notifications, "report");
    const palette = vi.fn();
    const newTab = vi.fn();
    const commands = [
      { id: COMMAND_IDS.showPalette, title: "Show Command Palette", run: palette },
      { id: COMMAND_IDS.newTab, title: "New Tab in Current Folder", run: newTab },
      { id: COMMAND_IDS.restartTaskDaemon, title: "Restart ctl-taskd", run: vi.fn() },
    ].map((command) => ({ ...command, category: "Test", enabled: true }));
    dispatcher.update(commands, true, vi.fn());
    const bindings = defaultKeybindings("macos");
    api.sync.mockResolvedValue({ rejected: [{
      command_id: COMMAND_IDS.restartTaskDaemon,
      index: 2,
      message: "Invalid native command descriptor.",
    }] });
    render(
      <NotificationProvider store={notifications}>
        <CommandProvider value={{ dispatcher, keybinding: (id) => bindings.get(id) }}>
          <CommandBindings platform="macos" />
        </CommandProvider>
      </NotificationProvider>,
    );
    await waitFor(() => expect(report).toHaveBeenLastCalledWith("native_shortcuts", {
      severity: "warning",
      title: "Some menu commands are unavailable",
      message: "Restart ctl-taskd: Invalid native command descriptor.",
      source: "Keyboard shortcuts",
    }));
    expect(api.sync).toHaveBeenLastCalledWith(expect.arrayContaining([
      expect.objectContaining({ command_id: COMMAND_IDS.showPalette, enabled: true, keybinding: { code: "KeyP", primary: true, shift: true } }),
      expect.objectContaining({ command_id: COMMAND_IDS.newTab, enabled: true, keybinding: { code: "KeyT", primary: true, shift: false } }),
    ]));
    fireEvent.keyDown(window, { code: "KeyP", metaKey: true, shiftKey: true });
    fireEvent.keyDown(window, { code: "KeyT", metaKey: true });
    expect(palette).not.toHaveBeenCalled();
    expect(newTab).not.toHaveBeenCalled();
    act(() => {
      api.listener!({ payload: COMMAND_IDS.showPalette });
      api.listener!({ payload: COMMAND_IDS.newTab });
    });
    expect(palette).toHaveBeenCalledOnce();
    expect(newTab).toHaveBeenCalledOnce();

    api.sync.mockResolvedValue({ rejected: [] });
    act(() => dispatcher.update(commands.map((command) => command.id === COMMAND_IDS.restartTaskDaemon
      ? { ...command, title: "Restart task daemon" } : command), true, vi.fn()));
    await waitFor(() => expect(report).toHaveBeenLastCalledWith("native_shortcuts", null));
  });

  it("reports whole-menu failures as errors without activating duplicate webview handling", async () => {
    const dispatcher = new CommandDispatcher();
    const notifications = new NotificationStore();
    const report = vi.spyOn(notifications, "report");
    const run = vi.fn();
    dispatcher.update([{ id: COMMAND_IDS.showPalette, title: "Show Command Palette", category: "Test", enabled: true, run }], true, vi.fn());
    const bindings = defaultKeybindings("macos");
    api.sync.mockRejectedValue(new Error("The application menu is unavailable."));
    render(
      <NotificationProvider store={notifications}>
        <CommandProvider value={{ dispatcher, keybinding: (id) => bindings.get(id) }}>
          <CommandBindings platform="macos" />
        </CommandProvider>
      </NotificationProvider>,
    );
    await waitFor(() => expect(report).toHaveBeenLastCalledWith("native_shortcuts", {
      severity: "error",
      title: "Native shortcuts",
      message: "Could not update native shortcuts: The application menu is unavailable.",
      source: "Keyboard shortcuts",
    }));
    fireEvent.keyDown(window, { code: "KeyP", metaKey: true, shiftKey: true });
    expect(run).not.toHaveBeenCalled();
    act(() => api.listener!({ payload: COMMAND_IDS.showPalette }));
    expect(run).toHaveBeenCalledOnce();
  });

  it("updates native accelerators from the keymap and never double-dispatches a native key", async () => {
    const dispatcher = new CommandDispatcher();
    const run = vi.fn();
    dispatcher.update(
      [
        {
          id: COMMAND_IDS.close,
          category: "Session",
          title: "Close",
          enabled: true,
          run,
        },
      ],
      true,
      vi.fn(),
    );
    const bindings = new Map(defaultKeybindings("macos"));
    const view = () => (
      <CommandProvider
        value={{ dispatcher, keybinding: (id) => bindings.get(id) }}
      >
        <CommandBindings platform="macos" />
      </CommandProvider>
    );
    const rendered = render(view());
    await waitFor(() =>
      expect(api.sync).toHaveBeenCalledWith([
        expect.objectContaining({
          command_id: COMMAND_IDS.close,
          keybinding: expect.objectContaining({ code: "KeyE" }),
        }),
      ]),
    );
    fireEvent.keyDown(window, { code: "KeyE", metaKey: true });
    expect(run).not.toHaveBeenCalled();
    act(() => api.listener!({ payload: COMMAND_IDS.close }));
    expect(run).toHaveBeenCalledOnce();
    bindings.set(COMMAND_IDS.close, { code: "KeyY", primary: true });
    rendered.rerender(view());
    await waitFor(() =>
      expect(api.sync).toHaveBeenLastCalledWith([
        expect.objectContaining({
          keybinding: { code: "KeyY", primary: true },
        }),
      ]),
    );
    bindings.delete(COMMAND_IDS.close);
    rendered.rerender(view());
    await waitFor(() =>
      expect(api.sync).toHaveBeenLastCalledWith([
        expect.objectContaining({ keybinding: null }),
      ]),
    );
  });

  it.each([false, true])("keeps Escape in the webview and native close scoped to its own confirmation (partial registration: %s)", async (partial_registration) => {
    if (partial_registration) api.sync.mockResolvedValue({ rejected: [{
      command_id: COMMAND_IDS.newShell,
      index: 1,
      message: "Could not create this menu item.",
    }] });
    const dispatcher = new CommandDispatcher();
    const appRun = vi.fn(),
      confirm = vi.fn(),
      cancel = vi.fn();
    dispatcher.update(
      [COMMAND_IDS.close, COMMAND_IDS.newShell].map((id) => ({
        id,
        title: id,
        category: "Test",
        enabled: true,
        run: appRun,
      })),
      true,
      vi.fn(),
    );
    const bindings = defaultKeybindings("macos");
    render(
      <CommandProvider
        value={{ dispatcher, keybinding: (id) => bindings.get(id) }}
      >
        <CommandBindings platform="macos" />
        <QuickInput
          title="Close session"
          mode={{ kind: "confirm", confirm_label: "Close", destructive: true }}
          confirm_command_id={COMMAND_IDS.close}
          onSubmit={confirm}
          onCancel={cancel}
        />
      </CommandProvider>,
    );
    await waitFor(() => expect(api.sync).toHaveBeenCalled());
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Escape",
      code: "Escape",
    });
    expect(cancel).toHaveBeenCalledOnce();
    act(() => api.listener!({ payload: COMMAND_IDS.newShell }));
    expect(appRun).not.toHaveBeenCalled();
    act(() => api.listener!({ payload: COMMAND_IDS.close }));
    expect(confirm).toHaveBeenCalledExactlyOnceWith("confirm");
    expect(appRun).not.toHaveBeenCalled();
  });
});
