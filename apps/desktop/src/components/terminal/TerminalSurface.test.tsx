// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TerminalSurface } from "./TerminalSurface";
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
