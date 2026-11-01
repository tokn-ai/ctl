// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TerminalSurface } from "./TerminalSurface";
import type { ComponentProps } from "react";
import type { TerminalSize } from "../../lib/types";
import { NotificationProvider } from "../../features/notifications/NotificationContext";
import { NotificationStore } from "../../features/notifications/NotificationStore";

const mocks = vi.hoisted(() => ({
  input: null as null | ((data: Uint8Array) => void),
  open_link: null as null | ((url: string) => void),
  copy_text: null as null | ((text: string) => void),
  open_external_url: vi.fn<(_: string) => Promise<void>>(),
  dispose: vi.fn(),
}));
vi.mock("../../lib/tauri", () => ({ openExternalUrl: mocks.open_external_url }));
vi.mock("../../features/terminal/XtermRenderer", () => ({ XtermRenderer: class {
  constructor(_container: HTMLElement, input: (data: Uint8Array) => void, _size: TerminalSize, open_link: (url: string) => void, copy_text: (text: string) => void) {
    mocks.input = input;
    mocks.open_link = open_link;
    mocks.copy_text = copy_text;
  }
  dispose = mocks.dispose;
} }));
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.unstubAllGlobals(); });

function renderSurface() {
  const store = new NotificationStore();
  const mounted = render(<NotificationProvider store={store}>
    <TerminalSurface phase="attached" hasSession has_cached_content onInput={vi.fn()} onReady={vi.fn()} />
  </NotificationProvider>);
  return { ...mounted, store };
}
describe("terminal exit acknowledgement", () => {
  it("retains the renderer, blocks PTY input, and consumes the dismissal key", () => {
    const input = vi.fn(), dismiss = vi.fn(), ready = vi.fn(), parent = vi.fn();
    const props = { hasSession: true, has_cached_content: true, onInput: input, onReady: ready, on_dismiss: dismiss };
    const mounted = render(<div onKeyDown={parent}><TerminalSurface {...props} phase="attached" /></div>);
    mounted.rerender(<div onKeyDown={parent}><TerminalSurface {...props} phase="ended" ended_message="Exited (code 7)" /></div>);
    expect(mocks.dispose).not.toHaveBeenCalled();
    mocks.input?.(new Uint8Array([65]));
    expect(input).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByRole("button", { name: "Close" }), { key: "a" });
    expect(dismiss).toHaveBeenCalledOnce();
    expect(parent).not.toHaveBeenCalled();
    expect(screen.getByRole("status").textContent).toContain("code 7");
  });
  it("does not treat a reconnecting transport as an exited session", () => {
    const dismiss = vi.fn();
    const mounted = render(<TerminalSurface phase="reconnecting" hasSession has_cached_content onInput={vi.fn()} onReady={vi.fn()} on_dismiss={dismiss} />);
    fireEvent.keyDown(mounted.container.firstChild!, { key: "a" });
    expect(dismiss).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Close" })).toBeNull();
  });
});

describe("disconnected terminal recovery", () => {
  function offline(overrides: Partial<ComponentProps<typeof TerminalSurface>> = {}) {
    const reconnect = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const interaction = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const input = vi.fn();
    const parent = vi.fn();
    const props = {
      phase: "disconnected" as const, hasSession: true, has_cached_content: true,
      onInput: input, onReady: vi.fn(), on_reconnect: reconnect, on_reconnect_interaction: interaction,
      ...overrides,
    };
    const mounted = render(<div onKeyDown={parent}><TerminalSurface {...props} /></div>);
    const terminal_input = document.createElement("textarea");
    terminal_input.setAttribute("aria-label", "Terminal input");
    mounted.container.querySelector(".terminal-container")!.append(terminal_input);
    return { ...mounted, props, reconnect, interaction, input, parent, terminal_input };
  }

  it.each([
    ["disconnected", "Disconnected"], ["error", "Disconnected"], ["retry_wait", "Reconnecting…"], ["reconnecting", "Reconnecting…"], ["connecting", "Connecting…"],
  ] as const)("shows %s beside cached output without a blocking overlay", (phase, label) => {
    const { container } = offline({ phase });
    expect(screen.getByRole("status").textContent).toContain(label);
    expect(container.querySelector(".terminal-overlay")).toBeNull();
    expect(container.querySelector(".terminal-disconnected")).not.toBeNull();
    expect(mocks.dispose).not.toHaveBeenCalled();
  });

  it("shows the paused state and lets the user explicitly reconnect", async () => {
    const { reconnect, interaction } = offline({ phase: "error", retry_paused: true, reconnect_error: "Automatic retries stopped." });
    expect(screen.getByRole("status").textContent).toContain("Retries paused");
    const button = screen.getByRole("button", { name: "Reconnect" });
    expect(button.title).toBe("Automatic retries stopped.");
    await act(async () => { fireEvent.click(button); });
    expect(reconnect).toHaveBeenCalledOnce();
    expect(interaction).not.toHaveBeenCalled();
  });

  it.each([
    { key: "a" }, { key: "A", shiftKey: true }, { key: "Enter" }, { key: "ArrowUp" }, { key: "Backspace" },
  ])("consumes $key to request quiet recovery without forwarding the key", async (key) => {
    const { terminal_input, interaction, reconnect, parent, input } = offline();
    const terminal_key = vi.fn();
    terminal_input.addEventListener("keydown", terminal_key);
    let allowed = true;
    await act(async () => { allowed = fireEvent.keyDown(terminal_input, key); });
    expect(allowed).toBe(false);
    expect(interaction).toHaveBeenCalledOnce();
    expect(reconnect).not.toHaveBeenCalled();
    expect(terminal_key).not.toHaveBeenCalled();
    expect(parent).not.toHaveBeenCalled();
    mocks.input?.(new Uint8Array([65]));
    expect(input).not.toHaveBeenCalled();
  });

  it("does not retry repeatedly while a key is held or a connection is already opening", () => {
    const { terminal_input, interaction, reconnect, props, rerender } = offline();
    expect(fireEvent.keyDown(terminal_input, { key: "a", repeat: true })).toBe(false);
    expect(interaction).not.toHaveBeenCalled();
    rerender(<TerminalSurface {...props} phase="reconnecting" />);
    const target = document.createElement("textarea");
    document.querySelector(".terminal-container")!.append(target);
    expect(fireEvent.keyDown(target, { key: "a" })).toBe(false);
    expect((screen.getByRole("button", { name: "Reconnect" }) as HTMLButtonElement).disabled).toBe(true);
    expect(interaction).not.toHaveBeenCalled();
    expect(reconnect).not.toHaveBeenCalled();
  });

  it.each([
    { key: "c", metaKey: true }, { key: "c", ctrlKey: true }, { key: "x", altKey: true },
    { key: "Shift", shiftKey: true }, { key: "Meta", metaKey: true }, { key: "PageUp", shiftKey: true },
    { key: "ArrowLeft", shiftKey: true }, { key: "あ", isComposing: true }, { key: "a", keyCode: 229 }, { key: "Dead" },
  ])("leaves local shortcuts, selection, and composition available: %j", (key) => {
    const { terminal_input, interaction, reconnect } = offline();
    expect(fireEvent.keyDown(terminal_input, key)).toBe(true);
    expect(interaction).not.toHaveBeenCalled();
    expect(reconnect).not.toHaveBeenCalled();
  });

  it("leaves scroll, pointer selection, focus, and reconnect-button keyboard use local", () => {
    const { terminal_input, interaction, reconnect } = offline();
    fireEvent.wheel(terminal_input, { deltaY: -100 });
    fireEvent.mouseDown(terminal_input, { metaKey: true, buttons: 1 });
    fireEvent.mouseMove(terminal_input, { metaKey: true, buttons: 1, clientX: 40 });
    fireEvent.mouseUp(terminal_input, { metaKey: true });
    fireEvent.focus(terminal_input);
    expect(fireEvent.keyDown(screen.getByRole("button", { name: "Reconnect" }), { key: "Enter" })).toBe(true);
    expect(interaction).not.toHaveBeenCalled();
    expect(reconnect).not.toHaveBeenCalled();
  });

  it("respects unavailable or temporarily disabled recovery", () => {
    const { terminal_input, props, rerender, interaction, reconnect } = offline({ reconnect_available: false });
    expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
    fireEvent.keyDown(terminal_input, { key: "a" });
    expect(interaction).not.toHaveBeenCalled();
    rerender(<TerminalSurface {...props} reconnect_available interaction_enabled={false} />);
    const target = document.createElement("textarea");
    document.querySelector(".terminal-container")!.append(target);
    expect(fireEvent.keyDown(target, { key: "a" })).toBe(true);
    expect((screen.getByRole("button", { name: "Reconnect" }) as HTMLButtonElement).disabled).toBe(true);
    expect(interaction).not.toHaveBeenCalled();
    expect(reconnect).not.toHaveBeenCalled();
  });

  it("restores input and removes the connection signal without recreating the renderer", () => {
    const { props, input, rerender } = offline();
    rerender(<div><TerminalSurface {...props} phase="attached" /></div>);
    expect(screen.queryByRole("status")).toBeNull();
    expect(mocks.dispose).not.toHaveBeenCalled();
    mocks.input?.(new Uint8Array([65]));
    expect(input).toHaveBeenCalledExactlyOnceWith(new Uint8Array([65]));
  });

  it("leaves ended and empty terminals outside connection recovery", () => {
    const dismiss = vi.fn();
    const { props, interaction, reconnect, rerender } = offline({ phase: "ended", ended_message: "Terminal exited", on_dismiss: dismiss });
    expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
    fireEvent.keyDown(screen.getByRole("button", { name: "Close" }), { key: "a" });
    expect(dismiss).toHaveBeenCalledOnce();
    rerender(<TerminalSurface {...props} phase="error" hasSession={false} ended_message={null} />);
    expect(screen.queryByRole("status")).toBeNull();
    expect(interaction).not.toHaveBeenCalled();
    expect(reconnect).not.toHaveBeenCalled();
  });
});

describe("terminal browser links", () => {
  it("opens the detected URL and reports a failed launch through existing notifications", async () => {
    const { store } = renderSurface();
    const url = "https://example.com/docs?q=one&next=two#links";
    mocks.open_external_url.mockRejectedValueOnce({ code: "browser_open_failed", message: "Browser unavailable." });
    await act(async () => { mocks.open_link?.(url); });
    expect(mocks.open_external_url).toHaveBeenCalledExactlyOnceWith(url);
    expect(store.snapshot().entries).toEqual([expect.objectContaining({
      title: "Could not open link", message: "Browser unavailable.", severity: "error", toast_visible: true,
    })]);

    mocks.open_external_url.mockResolvedValueOnce();
    await act(async () => { mocks.open_link?.(url); });
    expect(store.snapshot().entries[0].toast_visible).toBe(false);
    expect(store.snapshot().entries[0].resolved_at).not.toBeNull();
    expect(mocks.dispose).not.toHaveBeenCalled();
  });

  it("ignores a stale failure after a newer launch succeeds", async () => {
    const { store } = renderSurface();
    let reject_first!: (error: Error) => void;
    mocks.open_external_url.mockReturnValueOnce(new Promise<void>((_resolve, reject) => { reject_first = reject; }));
    mocks.open_external_url.mockResolvedValueOnce();
    await act(async () => {
      mocks.open_link?.("https://example.com/first");
      mocks.open_link?.("https://example.com/second");
    });
    await act(async () => { reject_first(new Error("First launch failed.")); });
    expect(store.snapshot().entries).toHaveLength(0);
  });

  it("does not report a late failure after the terminal is disposed", async () => {
    const { store, unmount } = renderSurface();
    let reject_open!: (error: Error) => void;
    mocks.open_external_url.mockReturnValueOnce(new Promise<void>((_resolve, reject) => { reject_open = reject; }));
    await act(async () => { mocks.open_link?.("https://example.com/"); });
    unmount();
    await act(async () => { reject_open(new Error("Launch failed.")); });
    expect(store.snapshot().entries).toHaveLength(0);
    expect(mocks.dispose).toHaveBeenCalledOnce();
  });
});

describe("terminal selection copying", () => {
  function clipboard() {
    const write_text = vi.fn<(_: string) => Promise<void>>().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText: write_text } });
    return write_text;
  }

  it("copies nonempty text immediately, preserves whitespace, and stays quiet on success", async () => {
    const write_text = clipboard();
    const { store } = renderSurface();
    const text = "  first line\nsecond line  ";
    await act(async () => {
      mocks.copy_text?.("");
      expect(write_text).not.toHaveBeenCalled();
      mocks.copy_text?.(text);
      expect(write_text).toHaveBeenCalledExactlyOnceWith(text);
    });
    expect(store.snapshot().entries).toHaveLength(0);
    expect(mocks.dispose).not.toHaveBeenCalled();
  });

  it("reports clipboard failure independently of browser links and resolves it after a successful copy", async () => {
    const write_text = clipboard();
    const { store } = renderSurface();
    write_text.mockRejectedValueOnce(new Error("Clipboard permission denied."));
    await act(async () => { mocks.copy_text?.("selected text"); });
    expect(store.snapshot().entries).toEqual([expect.objectContaining({
      title: "Could not copy selection", message: "Clipboard permission denied.", severity: "error", toast_visible: true,
    })]);

    mocks.open_external_url.mockResolvedValueOnce();
    await act(async () => { mocks.open_link?.("https://example.com/"); });
    expect(store.snapshot().entries[0].resolved_at).toBeNull();
    expect(store.snapshot().entries[0].toast_visible).toBe(true);

    await act(async () => { mocks.copy_text?.("selected text"); });
    expect(store.snapshot().entries[0].resolved_at).not.toBeNull();
    expect(store.snapshot().entries[0].toast_visible).toBe(false);
    expect(mocks.dispose).not.toHaveBeenCalled();
  });

  it("reports unavailable clipboard access without throwing from the gesture", async () => {
    vi.stubGlobal("navigator", {});
    const { store } = renderSurface();
    await act(async () => { mocks.copy_text?.("selected text"); });
    expect(store.snapshot().entries).toEqual([expect.objectContaining({
      title: "Could not copy selection", message: "Clipboard access is unavailable.",
    })]);
  });

  it("ignores a stale failure after a newer copy succeeds", async () => {
    const write_text = clipboard();
    const { store } = renderSurface();
    let reject_first!: (error: Error) => void;
    write_text.mockReturnValueOnce(new Promise<void>((_resolve, reject) => { reject_first = reject; }));
    await act(async () => {
      mocks.copy_text?.("first selection");
      mocks.copy_text?.("second selection");
    });
    await act(async () => { reject_first(new Error("First copy failed.")); });
    expect(store.snapshot().entries).toHaveLength(0);
  });

  it("does not let a stale success clear a newer copy failure", async () => {
    const write_text = clipboard();
    const { store } = renderSurface();
    let resolve_first!: () => void;
    write_text.mockReturnValueOnce(new Promise<void>((resolve) => { resolve_first = resolve; }));
    write_text.mockRejectedValueOnce(new Error("Second copy failed."));
    await act(async () => {
      mocks.copy_text?.("first selection");
      mocks.copy_text?.("second selection");
    });
    await act(async () => { resolve_first(); });
    expect(store.snapshot().entries).toEqual([expect.objectContaining({
      message: "Second copy failed.", resolved_at: null, toast_visible: true,
    })]);
  });

  it("ignores late results and callbacks after the terminal is disposed", async () => {
    const write_text = clipboard();
    const { store, unmount } = renderSurface();
    let reject_copy!: (error: Error) => void;
    write_text.mockReturnValueOnce(new Promise<void>((_resolve, reject) => { reject_copy = reject; }));
    await act(async () => { mocks.copy_text?.("selected text"); });
    unmount();
    await act(async () => {
      reject_copy(new Error("Copy failed."));
      mocks.copy_text?.("stale selection");
    });
    expect(write_text).toHaveBeenCalledExactlyOnceWith("selected text");
    expect(store.snapshot().entries).toHaveLength(0);
    expect(mocks.dispose).toHaveBeenCalledOnce();
  });
});
