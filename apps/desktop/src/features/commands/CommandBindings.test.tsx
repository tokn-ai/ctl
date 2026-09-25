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
  api.sync.mockResolvedValue(undefined);
  vi.stubGlobal("__TAURI_INTERNALS__", {});
  vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("native and webview command adapters", () => {
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

  it("keeps Escape in the webview and native close scoped to its own confirmation", async () => {
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
