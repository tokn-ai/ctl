// @vitest-environment jsdom
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Terminal, type ILink, type ILinkProvider } from "@xterm/xterm";
import { XtermRenderer } from "./XtermRenderer";
import type { SessionSummary, TerminalSize } from "../../lib/types";

const original_get_context = vi.hoisted(() => {
  // xterm's import-time CSS color fallback permits a missing canvas context.
  const original = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = (() => null) as typeof HTMLCanvasElement.prototype.getContext;
  return original;
});

afterAll(() => { HTMLCanvasElement.prototype.getContext = original_get_context; });

const size: TerminalSize = { columns: 20, rows: 2, pixel_width: null, pixel_height: null };
const encoder = new TextEncoder();
let container: HTMLElement;
let renderer: XtermRenderer;
let terminals: Terminal[];
let link_providers: Map<Terminal, ILinkProvider[]>;
let on_input: ReturnType<typeof vi.fn<(data: Uint8Array) => void>>;
let on_open_link: ReturnType<typeof vi.fn<(url: string) => void>>;
let on_copy_text: ReturnType<typeof vi.fn<(text: string) => void>>;

function measurable(element: HTMLElement): boolean {
  return element.isConnected && !element.closest("[hidden]") &&
    ![element, ...ancestors(element)].some((candidate) => candidate.style.display === "none");
}

function ancestors(element: HTMLElement): HTMLElement[] {
  const result: HTMLElement[] = [];
  for (let parent = element.parentElement; parent; parent = parent.parentElement) result.push(parent);
  return result;
}

beforeEach(() => {
  terminals = [];
  link_providers = new Map();
  on_input = vi.fn();
  on_open_link = vi.fn();
  on_copy_text = vi.fn();
  vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
  const register = Terminal.prototype.registerLinkProvider;
  vi.spyOn(Terminal.prototype, "registerLinkProvider").mockImplementation(function (this: Terminal, provider: ILinkProvider) {
    link_providers.set(this, [...(link_providers.get(this) ?? []), provider]);
    return register.call(this, provider);
  });
  const open = Terminal.prototype.open;
  vi.spyOn(Terminal.prototype, "open").mockImplementation(function (this: Terminal, parent: HTMLElement) {
    terminals.push(this);
    open.call(this, parent);
  });
  // Exercise xterm's real DOM renderer. Canvas glyph metrics remain available
  // while DOM WidthCache measurements require a connected, laid-out element.
  vi.stubGlobal("OffscreenCanvas", class {
    getContext() {
      return { font: "", measureText: (text: string) => ({ width: text.length * 8, fontBoundingBoxAscent: 11, fontBoundingBoxDescent: 2 }) };
    }
  });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => setTimeout(() => callback(performance.now()), 0));
  vi.stubGlobal("cancelAnimationFrame", (id: number) => clearTimeout(id));
  vi.stubGlobal("matchMedia", () => ({ matches: false, addListener() {}, removeListener() {} }));
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("xterm-char-measure-element") && measurable(this) ? (this.textContent?.length ?? 0) * 8 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) { return measurable(this) ? 13 : 0; });
  container = document.createElement("div");
  document.body.append(container);
  renderer = new XtermRenderer(container, on_input, size, on_open_link, on_copy_text);
});

afterEach(async () => {
  renderer.dispose();
  await Promise.resolve();
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function rowContainer(): HTMLElement {
  return container.querySelector<HTMLElement>(".xterm-rows")!;
}

function activeTerminal(owner_container: HTMLElement = container): Terminal {
  return terminals.find((terminal) => terminal.element && owner_container.contains(terminal.element))!;
}

function expectVisibleRows(): void {
  const terminal = activeTerminal();
  const buffer = terminal.buffer.active;
  expect([...rowContainer().children].map((row) => row.textContent?.trimEnd())).toEqual(
    Array.from({ length: terminal.rows }, (_, index) => (buffer.getLine(buffer.viewportY + index)?.translateToString(true) ?? "").trimEnd()),
  );
}

function linksAt(terminal: Terminal, line: number): ILink[] {
  const links: ILink[] = [];
  for (const provider of link_providers.get(terminal) ?? []) provider.provideLinks(line, (found) => links.push(...found ?? []));
  return links;
}

function terminalMouse(terminal: Terminal) {
  const element = terminal.element!;
  const screen = element.querySelector<HTMLElement>(".xterm-screen")!;
  const width = parseFloat(screen.style.width);
  const height = parseFloat(screen.style.height);
  element.style.padding = "0px";
  screen.style.padding = "0px";
  vi.spyOn(element, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, width, height));
  vi.spyOn(screen, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, width, height));
  return (type: string, column: number, row = 1, init: MouseEventInit = {}) => {
    screen.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true,
      clientX: (column - 0.5) * width / terminal.cols, clientY: (row - 0.5) * height / terminal.rows, ...init }));
  };
}

function expectLocalSelection(terminal: Terminal): void {
  expect(terminal.element!.querySelector(".terminal-selection-overlay")).not.toBeNull();
  expect(terminal.element!.querySelectorAll(".terminal-selection-rect").length).toBeGreaterThan(0);
  expect(terminal.hasSelection()).toBe(false);
}

function expectNoSelection(terminal: Terminal): void {
  expect(terminal.element!.querySelector(".terminal-selection-overlay")).toBeNull();
  expect(terminal.hasSelection()).toBe(false);
}

describe("retained terminal content", () => {
  it("retains readable content after the safe replay cursor is invalidated", async () => {
    expect(renderer.hasContent()).toBe(false);
    await renderer.resize(size);
    expect(renderer.hasContent()).toBe(false);
    await renderer.write(encoder.encode("last known screen"), "17", "0");
    renderer.invalidateResumeSequence();
    expect(renderer.resumeSequence()).toBeNull();
    expect(renderer.hasContent()).toBe(true);
    expect(activeTerminal().buffer.active.getLine(0)?.translateToString(true)).toBe("last known screen");
    await renderer.recreate(size);
    expect(renderer.hasContent()).toBe(false);
  });

  it("reports content for each cached session independently of the selected tab", async () => {
    const session: SessionSummary = { target: { kind: "local" }, session_id: "background", name: "background",
      status: "running", terminal_size: size, next_sequence: "0" };
    const background = renderer.sessionRenderer(session);
    await background.restoreCheckpoint(size, [], encoder.encode("cached background"), new Uint8Array(), "0");
    background.invalidateResumeSequence();
    expect(background.hasContent?.()).toBe(true);
    expect(renderer.hasContent()).toBe(false);
    renderer.selectSession(session);
    expect(renderer.hasContent()).toBe(true);
    expect(renderer.resumeSequence()).toBeNull();
  });
});

describe("terminal links", () => {
  it("detects naturally wrapped URLs with wide Unicode prefixes and surrounding punctuation", async () => {
    const url = "https://example.com/docs?q=one#part";
    await renderer.write(encoder.encode(`界 (${url}),`), "1", "0");
    const terminal = activeTerminal();
    expect(terminal.buffer.active.getLine(1)?.isWrapped).toBe(true);
    for (const line of [1, 2]) {
      expect(linksAt(terminal, line).map(({ text, range }) => ({ text, range }))).toEqual([
        { text: url, range: { start: { x: 5, y: 1 }, end: { x: 19, y: 2 } } },
      ]);
    }
    // The final punctuation wraps again and scrolls the first URL row away.
    // Clicking its continuation must account for the viewport's buffer offset.
    expect(terminal.buffer.active.viewportY).toBe(1);
    const mouse = terminalMouse(terminal);
    mouse("mousemove", 2);
    mouse("mousedown", 2, 1, { metaKey: true, buttons: 1 });
    mouse("mouseup", 2, 1, { metaKey: true });
    expect(on_open_link).toHaveBeenCalledExactlyOnceWith(url);
    terminal.scrollToTop();
    mouse("mousemove", 2, 2);
    mouse("mousedown", 2, 2, { metaKey: true, buttons: 1 });
    mouse("mouseup", 2, 2, { metaKey: true });
    expect(on_open_link).toHaveBeenCalledTimes(2);
  });

  it("keeps ordinary text selection over detected URLs", async () => {
    await renderer.write(encoder.encode("https://example.com"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousemove", 2);
    mouse("mousedown", 2, 1, { buttons: 1, detail: 1 });
    mouse("mousemove", 8, 1, { buttons: 1 });
    mouse("mouseup", 8);
    expect(terminal.getSelection().length).toBeGreaterThan(0);
    expect(on_open_link).not.toHaveBeenCalled();
  });

  it("uses the same modifier gesture for live OSC 8 links", async () => {
    const url = "https://example.com/details";
    await renderer.write(encoder.encode(`\u001b]8;;${url}\u0007Details\u001b]8;;\u0007`), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousemove", 2);
    expect(terminal.element!.title).toBe(`Cmd-click to open\n${url}`);
    mouse("mousedown", 2, 1, { buttons: 1 });
    mouse("mouseup", 2);
    expect(on_open_link).not.toHaveBeenCalled();
    mouse("mousedown", 2, 1, { metaKey: true, buttons: 1 });
    mouse("mouseup", 2, 1, { metaKey: true });
    expect(on_open_link).toHaveBeenCalledExactlyOnceWith(url);
  });

  it.each([1000, 1002, 1003])("consumes Cmd-click without sending PTY mouse reports in mode %s", async (mode) => {
    const url = "https://example.com";
    await renderer.write(encoder.encode(`${url}\u001b[?${mode}h\u001b[?1006h`), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousemove", 2);
    expect(terminal.element!.title).toContain(url);
    on_input.mockClear();
    mouse("mousedown", 2, 1, { metaKey: true, buttons: 1 });
    mouse("mousemove", 2, 1, { metaKey: true, buttons: 1 });
    mouse("mouseup", 2, 1, { metaKey: true });
    expect(on_open_link).toHaveBeenCalledExactlyOnceWith(url);
    expect(on_input).not.toHaveBeenCalled();
    mouse("mousedown", 2, 1, { buttons: 1 });
    mouse("mouseup", 2);
    expect(on_input.mock.calls.map(([data]) => new TextDecoder().decode(data))).toEqual(["\u001b[<0;2;1M", "\u001b[<0;2;1m"]);
    expect(on_open_link).toHaveBeenCalledOnce();
  });

  it("installs links on recreated and staged history adapters and releases the previous adapter", async () => {
    const original = activeTerminal();
    await renderer.recreate(size);
    expect(activeTerminal()).not.toBe(original);
    const url = "https://example.com";
    await renderer.restoreCheckpoint(size, [], encoder.encode(url), new Uint8Array(), "0", "links");
    const previous = activeTerminal();
    const dispose = vi.spyOn(previous, "dispose");
    expect(await renderer.syncHistory({ terminal_size: size, rows: [{ text: url, wrapped: false }],
      payload: encoder.encode(url), input_prefix: new Uint8Array(), sequence: "0", snapshot_id: "links", scrollback_limit: "10000" })).toBe(true);
    const terminal = activeTerminal();
    expect(terminal).not.toBe(previous);
    expect(dispose).toHaveBeenCalledOnce();
    expect(linksAt(terminal, 1).map((link) => link.text)).toEqual([url]);
    expect(linksAt(terminal, 2).map((link) => link.text)).toEqual([url]);
    const mouse = terminalMouse(terminal);
    mouse("mousemove", 2);
    mouse("mousedown", 2, 1, { metaKey: true, buttons: 1 });
    mouse("mouseup", 2, 1, { metaKey: true });
    expect(on_open_link).toHaveBeenCalledExactlyOnceWith(url);
  });
});

describe("modified terminal selection", () => {
  beforeEach(() => {
    // Exercise the Ctrl gesture here; the separate browser-bundle suite covers
    // macOS Cmd gestures with xterm's actual browser platform detection.
    vi.spyOn(navigator, "platform", "get").mockReturnValue("Linux x86_64");
    renderer.dispose();
    renderer = new XtermRenderer(container, on_input, size, on_open_link, on_copy_text);
  });

  it.each(["normal", "alternate"].flatMap((buffer) => [null, 9, 1000, 1002, 1003].map((mode) => ({ buffer, mode }))))(
    "copies once without PTY input in $buffer buffer with mouse mode $mode", async ({ buffer, mode }) => {
      const buffer_control = buffer === "alternate" ? "\u001b[?1049h" : "";
      const mouse_control = mode === null ? "" : `\u001b[?${mode}h\u001b[?1006h`;
      await renderer.write(encoder.encode(`${buffer_control}${mouse_control}alpha beta gamma`), "1", "0");
      const terminal = activeTerminal();
      expect(terminal.buffer.active.type).toBe(buffer);
      const mouse = terminalMouse(terminal);
      mouse("mousemove", 1);
      on_input.mockClear();
      mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
      expectNoSelection(terminal);
      mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
      expectLocalSelection(terminal);
      mouse("mouseup", 11, 1, { ctrlKey: true });
      mouse("mouseup", 11, 1, { ctrlKey: true });
      expectNoSelection(terminal);
      expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
      expect(on_open_link).not.toHaveBeenCalled();
      expect(on_input).not.toHaveBeenCalled();
    },
  );

  it("copies a reverse drag after the modifier is released", async () => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
    const mouse = terminalMouse(activeTerminal());
    mouse("mousedown", 11, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 1, 1, { buttons: 1 });
    mouse("mouseup", 1);
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expectNoSelection(activeTerminal());
    expect(on_input).not.toHaveBeenCalled();
  });

  it("keeps modifier hover silent before and after selecting in ANY mouse mode", async () => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousemove", 1, 1, { ctrlKey: true });
    expect(on_input).not.toHaveBeenCalled();
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
    mouse("mousemove", 12, 1, { ctrlKey: true });
    expectNoSelection(terminal);
    expect(on_input).not.toHaveBeenCalled();
    // Releasing the modifier restores the program's ordinary mouse behavior.
    mouse("mousemove", 13);
    expect(on_input.mock.calls.map(([data]) => new TextDecoder().decode(data))).toEqual(["\u001b[<35;13;1M"]);
  });

  it("keeps wheel input local during a captured drag and still copies on release", async () => {
    await renderer.write(encoder.encode("older 0\r\nolder 1\r\nalpha beta gamma\r\nbottom row\u001b[?1003h\u001b[?1006h"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    expectLocalSelection(terminal);
    // The captured gesture stays local even if Ctrl is released while scrolling.
    terminal.element!.querySelector(".xterm-screen")!.dispatchEvent(new WheelEvent("wheel", {
      bubbles: true, cancelable: true, clientX: 84, clientY: 8, buttons: 1, deltaY: -30,
    }));
    expect(on_input).not.toHaveBeenCalled();
    expectLocalSelection(terminal);
    mouse("mouseup", 11);
    expect(on_copy_text).toHaveBeenCalledOnce();
    expectNoSelection(terminal);
    expect(on_copy_text.mock.calls[0][0].length).toBeGreaterThan(0);
    expect(on_input).not.toHaveBeenCalled();
  });

  it("keeps native focus reports silent throughout a modified gesture", async () => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1004h\u001b[?1006h"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    terminal.blur();
    on_input.mockClear();
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expect(on_input).not.toHaveBeenCalled();
    expect(terminal.options.disableStdin).toBe(false);
    terminal.blur();
    on_input.mockClear();
    mouse("mousedown", 1, 1, { buttons: 1 });
    mouse("mouseup", 1);
    expect(on_input.mock.calls.some(([data]) => new TextDecoder().decode(data) === "\u001b[I")).toBe(true);
  });

  it("keeps stale mouse listeners and focus reports in another pane silent", async () => {
    const other_container = document.createElement("div");
    document.body.append(other_container);
    const other_input = vi.fn<(data: Uint8Array) => void>();
    const other_renderer = new XtermRenderer(other_container, other_input, size);
    try {
      await other_renderer.write(encoder.encode("other pane\u001b[?1002h\u001b[?1004h\u001b[?1006h"), "1", "0");
      const other_terminal = activeTerminal(other_container);
      const other_mouse = terminalMouse(other_terminal);
      other_mouse("mousedown", 1, 1, { buttons: 1 });
      expect(other_input).toHaveBeenCalled();
      // The lost release leaves document-level PTY listeners owned by this pane.
      window.dispatchEvent(new Event("blur"));
      other_input.mockClear();
      await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
      const mouse = terminalMouse(activeTerminal());
      mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
      mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
      mouse("mouseup", 11, 1, { ctrlKey: true });
      expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
      expectNoSelection(activeTerminal());
      expect(on_input).not.toHaveBeenCalled();
      expect(other_input).not.toHaveBeenCalled();
      expect(other_terminal.options.disableStdin).toBe(false);
      // Silencing a selection must not disable later interaction with either pane.
      other_mouse("mousedown", 2, 1, { buttons: 1 });
      other_mouse("mouseup", 2);
      expect(other_input).toHaveBeenCalled();
    } finally {
      other_renderer.dispose();
      other_container.remove();
    }
  });

  it("clears an unfinished native selection in another pane before starting its own drag", async () => {
    const other_container = document.createElement("div");
    document.body.append(other_container);
    const other_input = vi.fn<(data: Uint8Array) => void>();
    const other_renderer = new XtermRenderer(other_container, other_input, size);
    try {
      await other_renderer.write(encoder.encode("other pane words"), "1", "0");
      const other_terminal = activeTerminal(other_container);
      const other_mouse = terminalMouse(other_terminal);
      other_mouse("mousedown", 1, 1, { buttons: 1, detail: 1 });
      other_mouse("mousemove", 6, 1, { buttons: 1 });
      expect(other_terminal.getSelection()).toBe("other");
      // Leave the native drag unfinished, as when the pointer release is lost.
      await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
      const terminal = activeTerminal();
      const mouse = terminalMouse(terminal);
      mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
      expectNoSelection(other_terminal);
      mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
      expectLocalSelection(terminal);
      mouse("mouseup", 11, 1, { ctrlKey: true });
      expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
      expectNoSelection(terminal);
      other_mouse("mousemove", 12, 1, { ctrlKey: true });
      expectNoSelection(other_terminal);
      expect(on_input).not.toHaveBeenCalled();
      expect(other_input).not.toHaveBeenCalled();
    } finally {
      other_renderer.dispose();
      other_container.remove();
    }
  });

  it("scrolls only the pane owning a local drag when wheel input targets another pane", async () => {
    const content = Array.from({ length: 6 }, (_, index) => `line ${index} selection`).join("\r\n");
    await renderer.write(encoder.encode(`${content}\u001b[?1003h\u001b[?1006h`), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    const other_container = document.createElement("div");
    document.body.append(other_container);
    const other_input = vi.fn<(data: Uint8Array) => void>();
    const other_renderer = new XtermRenderer(other_container, other_input, size);
    try {
      await other_renderer.write(encoder.encode(`${content}\u001b[?1003h\u001b[?1006h`), "1", "0");
      const other_terminal = activeTerminal(other_container);
      terminalMouse(other_terminal);
      const source_offset = terminal.buffer.active.viewportY;
      const other_offset = other_terminal.buffer.active.viewportY;
      expect(source_offset).toBeGreaterThan(0);
      expect(other_offset).toBeGreaterThan(0);
      mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
      mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
      other_terminal.element!.querySelector(".xterm-screen")!.dispatchEvent(new WheelEvent("wheel", {
        bubbles: true, cancelable: true, clientX: 84, clientY: 8,
        ctrlKey: true, buttons: 1, deltaY: -15,
      }));
      expect(terminal.buffer.active.viewportY).toBeLessThan(source_offset);
      expect(other_terminal.buffer.active.viewportY).toBe(other_offset);
      expect(on_input).not.toHaveBeenCalled();
      expect(other_input).not.toHaveBeenCalled();
      mouse("mouseup", 11, 1, { ctrlKey: true });
      expect(on_copy_text).toHaveBeenCalledOnce();
      expectNoSelection(terminal);
      expect(other_input).not.toHaveBeenCalled();
    } finally {
      other_renderer.dispose();
      other_container.remove();
    }
  });

  it.each([1002, 1003])("does not leak input through stale TUI mouse listeners in mode %s", async (mode) => {
    await renderer.write(encoder.encode(`alpha beta gamma\r\nsecond row\r\nthird row\r\nbottom row\u001b[?${mode}h\u001b[?1006h`), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { buttons: 1 });
    expect(on_input).toHaveBeenCalled();
    // The release occurred outside the window, so xterm never received it.
    window.dispatchEvent(new Event("blur"));
    terminal.scrollToTop();
    expect(terminal.buffer.active.viewportY).toBe(0);
    on_input.mockClear();
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expectNoSelection(terminal);
    expect(on_input).not.toHaveBeenCalled();
    expect(terminal.buffer.active.viewportY).toBe(0);
  });

  it.each(["resize", "buffer change", "blur"])("owns a cancelled local release after %s despite a stale PTY listener", async (reason) => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1002h\u001b[?1006h"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { buttons: 1 });
    expect(on_input).toHaveBeenCalled();
    // Releasing outside the window leaves xterm's document PTY listener alive.
    window.dispatchEvent(new Event("blur"));
    on_input.mockClear();
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    expect(container.querySelector(".terminal-selection-overlay")).not.toBeNull();
    expect(on_input).not.toHaveBeenCalled();
    if (reason === "resize") await renderer.resize({ ...size, columns: 18 });
    else if (reason === "buffer change") await renderer.write(encoder.encode("\u001b[?1049h"), "2", "1");
    else window.dispatchEvent(new Event("blur"));
    expect(container.querySelector(".terminal-selection-overlay")).toBeNull();
    mouse("mousemove", 12, 1, { buttons: 1 });
    expect(container.querySelector(".terminal-selection-overlay")).toBeNull();
    expect(on_input).not.toHaveBeenCalled();
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expect(on_input).not.toHaveBeenCalled();
    expect(on_copy_text).not.toHaveBeenCalled();
    expect(on_open_link).not.toHaveBeenCalled();
    // Cancellation must relinquish ownership after the physical release.
    mouse("mousedown", 1, 1, { buttons: 1 });
    mouse("mouseup", 1);
    expect(on_input).toHaveBeenCalled();
  });

  it("ends cancelled ownership on the next primary press outside the terminal", async () => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
    const mouse = terminalMouse(activeTerminal());
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    window.dispatchEvent(new Event("blur"));
    const button = document.createElement("button");
    const down = vi.fn();
    button.addEventListener("mousedown", down);
    document.body.append(button);
    try {
      for (const type of ["mousedown", "mousemove", "mouseup"]) {
        const event = new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, buttons: type === "mouseup" ? 0 : 1 });
        button.dispatchEvent(event);
        expect(event.defaultPrevented).toBe(false);
      }
      expect(down).toHaveBeenCalledOnce();
      expect(on_copy_text).not.toHaveBeenCalled();
      expect(container.querySelector(".terminal-selection-overlay")).toBeNull();
      mouse("mousedown", 1, 1, { buttons: 1 });
      mouse("mouseup", 1);
      expect(on_input).toHaveBeenCalled();
    } finally {
      button.remove();
    }
  });

  it("owns the cancelled release after its renderer is disposed while another pane has stale PTY listeners", async () => {
    const other_container = document.createElement("div");
    document.body.append(other_container);
    const other_input = vi.fn<(data: Uint8Array) => void>();
    const other_renderer = new XtermRenderer(other_container, other_input, size);
    try {
      await other_renderer.write(encoder.encode("other pane\u001b[?1002h\u001b[?1006h"), "1", "0");
      const other_mouse = terminalMouse(activeTerminal(other_container));
      other_mouse("mousedown", 1, 1, { buttons: 1 });
      window.dispatchEvent(new Event("blur"));
      other_input.mockClear();
      await renderer.write(encoder.encode("alpha beta gamma"), "1", "0");
      const terminal = activeTerminal();
      const mouse = terminalMouse(terminal);
      mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
      mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
      const disposed = vi.spyOn(terminal, "dispose");
      renderer.dispose();
      await vi.waitFor(() => expect(disposed).toHaveBeenCalledOnce());
      document.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, clientX: 80, clientY: 8, buttons: 1 }));
      document.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, clientX: 80, clientY: 8 }));
      expect(on_copy_text).not.toHaveBeenCalled();
      expect(other_input).not.toHaveBeenCalled();
      other_mouse("mousedown", 1, 1, { buttons: 1 });
      other_mouse("mouseup", 1);
      expect(other_input).toHaveBeenCalled();
    } finally {
      other_renderer.dispose();
      other_container.remove();
    }
  });

  it("removes lost PTY mouse listeners before disposing their renderer", async () => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { buttons: 1 });
    expect(on_input).toHaveBeenCalled();
    on_input.mockClear();
    const disposed = vi.spyOn(terminal, "dispose");
    renderer.dispose();
    await vi.waitFor(() => expect(disposed).toHaveBeenCalledOnce());
    const errors: string[] = [];
    const error = (event: ErrorEvent) => { errors.push(event.message); event.preventDefault(); };
    window.addEventListener("error", error);
    try {
      document.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, clientX: 80, clientY: 8, buttons: 1 }));
      document.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, clientX: 80, clientY: 8 }));
      expect(errors).toEqual([]);
      expect(on_input).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener("error", error);
    }
  });

  it("uses the final release position when no intermediate mousemove arrives", async () => {
    await renderer.write(encoder.encode("alpha beta gamma"), "1", "0");
    const mouse = terminalMouse(activeTerminal());
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expectNoSelection(activeTerminal());
    expect(on_input).not.toHaveBeenCalled();
  });

  it("preserves Unicode and hard newlines while joining naturally wrapped rows", async () => {
    await renderer.resize({ ...size, columns: 8, rows: 4 });
    const text = "界e\u0301abcdEF\r\nsecond";
    await renderer.write(encoder.encode(text), "1", "0");
    const terminal = activeTerminal();
    expect(terminal.buffer.active.getLine(1)?.isWrapped).toBe(true);
    expect(terminal.buffer.active.getLine(2)?.isWrapped).toBe(false);
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 7, 3, { ctrlKey: true, buttons: 1 });
    expectLocalSelection(terminal);
    mouse("mouseup", 7, 3, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("界e\u0301abcdEF\nsecond");
    expectNoSelection(terminal);
    expect(on_input).not.toHaveBeenCalled();
  });

  it("copies a drag that starts on a link without opening that link", async () => {
    await renderer.write(encoder.encode("https://example.com"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousemove", 1);
    expect(terminal.element!.title).toContain("https://example.com");
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 9, 1, { ctrlKey: true, buttons: 1 });
    mouse("mouseup", 9, 1, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("https://");
    expectNoSelection(terminal);
    expect(on_open_link).not.toHaveBeenCalled();
    expect(on_input).not.toHaveBeenCalled();
  });

  it.each([false, true])("resets previous rectangular selection when mouse reporting is %s", async (reporting) => {
    await renderer.write(encoder.encode("alpha beta\r\nsecond row"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { altKey: true, buttons: 1, detail: 1 });
    mouse("mousemove", 5, 2, { altKey: true, buttons: 1 });
    mouse("mouseup", 5, 2, { altKey: true });
    expect(terminal.getSelection()).toBe("alph\nseco");
    expect(on_copy_text).not.toHaveBeenCalled();
    if (reporting) await renderer.write(encoder.encode("\u001b[?1003h\u001b[?1006h"), "2", "1");
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    expectNoSelection(terminal);
    mouse("mousemove", 7, 2, { ctrlKey: true, buttons: 1 });
    expectLocalSelection(terminal);
    mouse("mouseup", 7, 2, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta\nsecond");
    expectNoSelection(terminal);
    expect(on_input).not.toHaveBeenCalled();
  });

  it.each(["blur", "resize", "buffer change", "dispose"])("cancels an in-progress drag on %s", async (transition) => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    expectLocalSelection(terminal);
    if (transition === "blur") window.dispatchEvent(new Event("blur"));
    else if (transition === "resize") await renderer.resize({ ...size, columns: 18 });
    else if (transition === "buffer change") await renderer.write(encoder.encode("\u001b[?1049h"), "2", "1");
    else {
      renderer.dispose();
      // Presenter disposal waits for its current parser operation to settle.
      await vi.waitFor(() => expectNoSelection(terminal));
    }
    expectNoSelection(terminal);
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expect(on_copy_text).not.toHaveBeenCalled();
    expect(on_open_link).not.toHaveBeenCalled();
    expect(on_input).not.toHaveBeenCalled();
  });

  it("continues selecting through edge autoscroll and copies on release", async () => {
    await renderer.write(encoder.encode(Array.from({ length: 10 }, (_, index) => `line ${index}`).join("\r\n")), "1", "0");
    const terminal = activeTerminal();
    terminal.scrollToTop();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 7, 4, { ctrlKey: true, buttons: 1 });
    await vi.waitFor(() => expect(terminal.buffer.active.viewportY).toBeGreaterThan(0));
    expectLocalSelection(terminal);
    mouse("mouseup", 7, 4, { ctrlKey: true });
    expect(on_copy_text).toHaveBeenCalledOnce();
    expectNoSelection(terminal);
    expect(on_copy_text.mock.calls[0][0]).toMatch(/^line 0\nline 1\nline 2/);
    expect(on_input).not.toHaveBeenCalled();
  });

  it("reads copied text without using xterm selection APIs or forwarding drag events", async () => {
    await renderer.write(encoder.encode("alpha beta gamma\u001b[?1003h\u001b[?1006h"), "1", "0");
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    const read = vi.spyOn(terminal, "getSelection");
    const select = vi.spyOn(terminal, "select");
    const select_all = vi.spyOn(terminal, "selectAll");
    const select_lines = vi.spyOn(terminal, "selectLines");
    const delivered = vi.fn();
    for (const type of ["mousedown", "mousemove", "mouseup"]) terminal.element!.addEventListener(type, delivered);
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    expectNoSelection(terminal);
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    expectLocalSelection(terminal);
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expectNoSelection(terminal);
    expect(on_copy_text).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expect(delivered).not.toHaveBeenCalled();
    for (const method of [read, select, select_all, select_lines]) expect(method).not.toHaveBeenCalled();
    expect(on_input).not.toHaveBeenCalled();
  });

  it("does not show an overlay for a press or copy an empty drag", async () => {
    const terminal = activeTerminal();
    const mouse = terminalMouse(terminal);
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    expectNoSelection(terminal);
    mouse("mouseup", 1, 1, { ctrlKey: true });
    expectNoSelection(terminal);
    expect(on_copy_text).not.toHaveBeenCalled();
    mouse("mousedown", 1, 1, { ctrlKey: true, buttons: 1 });
    mouse("mousemove", 11, 1, { ctrlKey: true, buttons: 1 });
    mouse("mouseup", 11, 1, { ctrlKey: true });
    expectNoSelection(terminal);
    expect(on_copy_text).not.toHaveBeenCalled();
    expect(on_input).not.toHaveBeenCalled();
  });
});

describe("resizing with history", () => {
  it("measures rendered live history once per changed adapter and keeps primary history in alternate screen", async () => {
    await renderer.write(encoder.encode("one\r\ntwo\r\nlive"), "14", "0");
    const buffer = activeTerminal().buffer.normal;
    const read = vi.spyOn(buffer, "getLine");
    expect(renderer.historyBytes()).toBe(4);
    const reads = read.mock.calls.length;
    expect(reads).toBeGreaterThan(0);
    expect(renderer.historyBytes()).toBe(4);
    expect(read).toHaveBeenCalledTimes(reads);
    await renderer.write(encoder.encode("\r\nnext"), "20", "14");
    expect(renderer.historyBytes()).toBe(8);
    expect(read.mock.calls.length).toBeGreaterThan(reads);
    await renderer.write(encoder.encode("\u001b[?1049halt"), "31", "20");
    expect(renderer.historyBytes()).toBe(8);
    await renderer.write(encoder.encode("\u001b[?1049l\u001b[3J"), "43", "31");
    expect(renderer.historyBytes()).toBe(0);
  });

  it("measures the committed history adapter and invalidates measurements on resize", async () => {
    await renderer.restoreCheckpoint(size, [], encoder.encode("live"), new Uint8Array(), "0", "snapshot");
    expect(renderer.historyBytes()).toBe(0);
    expect(await renderer.syncHistory({ terminal_size: size, rows: [{ text: "older", wrapped: false }], payload: encoder.encode("live"),
      input_prefix: new Uint8Array(), sequence: "0", snapshot_id: "snapshot", scrollback_limit: "10000" })).toBe(true);
    expect(renderer.historyBytes()).toBe(6);
    const buffer = activeTerminal().buffer.normal;
    const read = vi.spyOn(buffer, "getLine");
    await renderer.resize({ ...size, columns: 18 });
    renderer.historyBytes();
    expect(read).toHaveBeenCalled();
  });

  it("paints the current viewport after repeated checkpoints, history activation and wheel scrolling", async () => {
    const history = Array.from({ length: 50 }, (_, index) => ({ text: `older ${index}`, wrapped: false }));
    const sizes = [{ columns: 60, rows: 6 }, { columns: 32, rows: 3 }, { columns: 80, rows: 8 }, { columns: 24, rows: 2 }];
    for (const [index, geometry] of sizes.entries()) {
      const current_size = { ...size, ...geometry };
      const prompt = `\u001b[2J\u001b[HRestored session: Wed Oct 7\u001b[2;1H\u001b[34mclouds\u001b[0m@host ~ % `;
      const payload = encoder.encode(prompt);
      const snapshot_id = `snapshot-${index}`;
      await renderer.restoreCheckpoint(current_size, history.map((row) => row.text), payload, new Uint8Array(), "0", snapshot_id);
      expect(await renderer.syncHistory({ terminal_size: current_size, rows: history, payload, input_prefix: new Uint8Array(),
        sequence: "0", snapshot_id, scrollback_limit: "10000" })).toBe(true);
      await vi.waitFor(expectVisibleRows);
      let terminal = activeTerminal();
      const screen = container.querySelector<HTMLElement>(".xterm-screen")!;
      screen.dispatchEvent(new WheelEvent("wheel", { bubbles: true, cancelable: true, deltaY: -120 }));
      await vi.waitFor(() => {
        expect(terminal.buffer.active.viewportY).toBeLessThan(terminal.buffer.active.baseY);
        expectVisibleRows();
      });
      await renderer.write(encoder.encode("cccclc"), "6", "0");
      await vi.waitFor(expectVisibleRows);
      terminal.scrollToBottom();
      await vi.waitFor(() => {
        expect(terminal.buffer.active.viewportY).toBe(terminal.buffer.active.baseY);
        expectVisibleRows();
        expect(rowContainer().children[1].textContent?.trimEnd()).toBe("clouds@host ~ % cccclc");
      });
      terminal.scrollLines(-3);
      await vi.waitFor(() => expect(terminal.buffer.active.viewportY).toBeLessThan(terminal.buffer.active.baseY));
    }
  });
});

describe("terminal font measurement", () => {
  it("opens the live terminal with measurable glyph widths", async () => {
    await renderer.write(encoder.encode("compact"), "7", "0");
    expect(rowContainer().style.letterSpacing).toBe("0px");
  });

  it("measures staged history without adding visual spaces when it becomes live", async () => {
    await renderer.restoreCheckpoint(size, [], encoder.encode("live"), new Uint8Array(), "0", "snapshot");
    const previous = rowContainer();
    const pending = renderer.syncHistory({ terminal_size: size, rows: [{ text: "older", wrapped: false }],
      payload: encoder.encode("live"), input_prefix: new Uint8Array(), sequence: "0", snapshot_id: "snapshot", scrollback_limit: "10000" });
    expect(rowContainer()).toBe(previous);
    expect(await pending).toBe(true);
    expect(rowContainer()).not.toBe(previous);
    await vi.waitFor(() => expect(rowContainer().textContent).toContain("live"));
    const glyphs = [...rowContainer().querySelectorAll<HTMLElement>("span")].filter((span) => span.textContent?.trim());
    expect(glyphs.length).toBeGreaterThan(0);
    expect(glyphs.map((span) => span.style.letterSpacing || rowContainer().style.letterSpacing)).toEqual(glyphs.map(() => "0px"));
    expect(rowContainer().style.letterSpacing).toBe("0px");
    expect(document.querySelectorAll(".terminal-measurement")).toHaveLength(0);
  });

  it("keeps the focused textarea and live DOM through replacing resize checkpoints", async () => {
    await renderer.restoreCheckpoint(size, [], encoder.encode("first"), new Uint8Array(), "0", "first");
    const previous = rowContainer();
    const textarea = container.querySelector<HTMLTextAreaElement>("textarea")!;
    renderer.focus();
    expect(document.activeElement).toBe(textarea);
    await renderer.restoreCheckpoint({ ...size, columns: 18 }, [], encoder.encode("next"), new Uint8Array([0xe2, 0x82]), "2", "next");
    await renderer.write(new Uint8Array([0xac]), "3", "2");
    expect(rowContainer()).toBe(previous);
    expect(container.querySelector("textarea")).toBe(textarea);
    expect(document.activeElement).toBe(textarea);
    await vi.waitFor(() => expect(rowContainer().textContent).toContain("next€"));
    expect(renderer.resumeSequence()).toBe("3");
  });

  it("measures history outside hidden session ancestors and preserves focus on activation", async () => {
    await renderer.restoreCheckpoint(size, [], encoder.encode("live"), new Uint8Array(), "0", "snapshot");
    const textarea = container.querySelector<HTMLTextAreaElement>("textarea")!;
    renderer.focus();
    container.hidden = true;
    const pending = renderer.syncHistory({ terminal_size: size, rows: [{ text: "older", wrapped: false }],
      payload: encoder.encode("live"), input_prefix: new Uint8Array(), sequence: "0", snapshot_id: "snapshot", scrollback_limit: "10000" });
    const host = document.querySelector<HTMLElement>(".terminal-measurement")!;
    expect(host.parentElement).toBe(document.body);
    expect(host.style.visibility).toBe("hidden");
    expect(host.style.display).not.toBe("none");
    container.hidden = false;
    expect(await pending).toBe(true);
    expect(document.activeElement).not.toBe(textarea);
    expect(document.activeElement).toBe(container.querySelector("textarea"));
    expect(rowContainer().style.letterSpacing).toBe("0px");
    expect(document.querySelectorAll(".terminal-measurement")).toHaveLength(0);
  });

  it.each(["cancel", "dispose"])("removes a pending history measurement host on %s", async (transition) => {
    await renderer.restoreCheckpoint(size, [], encoder.encode("live"), new Uint8Array(), "0", "snapshot");
    const previous = rowContainer();
    const pending = renderer.syncHistory({ terminal_size: size, rows: [{ text: "older", wrapped: false }],
      payload: encoder.encode("live"), input_prefix: new Uint8Array(), sequence: "0", snapshot_id: "snapshot", scrollback_limit: "10000" });
    expect(document.querySelectorAll(".terminal-measurement")).toHaveLength(1);
    if (transition === "cancel") renderer.cancelHistory();
    else renderer.dispose();
    expect(await pending).toBe(false);
    expect(document.querySelectorAll(".terminal-measurement")).toHaveLength(0);
    if (transition === "cancel") expect(rowContainer()).toBe(previous);
  });

  it("retains measured glyph spacing when a hidden cached session is selected", async () => {
    const session: SessionSummary = { target: { kind: "local" }, session_id: "background", name: "background",
      status: "running", terminal_size: size, next_sequence: "0" };
    const background = renderer.sessionRenderer(session);
    await background.restoreCheckpoint(size, [], encoder.encode("background"), new Uint8Array(), "0", "snapshot");
    expect(container.querySelector<HTMLElement>(".terminal-session[hidden]")).not.toBeNull();
    const pending = background.syncHistory!({ terminal_size: size, rows: [{ text: "older", wrapped: false }],
      payload: encoder.encode("background"), input_prefix: new Uint8Array(), sequence: "0", snapshot_id: "snapshot", scrollback_limit: "10000" });
    expect(await pending).toBe(true);
    renderer.selectSession(session);
    const selected = container.querySelector<HTMLElement>(".terminal-session:not([hidden]) .xterm-rows")!;
    await vi.waitFor(() => expect(selected.textContent).toContain("background"));
    const glyphs = [...selected.querySelectorAll<HTMLElement>("span")].filter((span) => span.textContent?.trim());
    expect(glyphs.map((span) => span.style.letterSpacing || selected.style.letterSpacing)).toEqual(glyphs.map(() => "0px"));
    expect(selected.style.letterSpacing).toBe("0px");
    expect(document.querySelectorAll(".terminal-measurement")).toHaveLength(0);
  });

  it("remeasures cached checkpoint glyphs when their hidden session becomes visible", async () => {
    const session: SessionSummary = { target: { kind: "local" }, session_id: "background", name: "background",
      status: "running", terminal_size: size, next_sequence: "0" };
    const background = renderer.sessionRenderer(session);
    await background.restoreCheckpoint(size, [], encoder.encode("background"), new Uint8Array(), "0", "snapshot");
    const rows = container.querySelector<HTMLElement>(".terminal-session[hidden] .xterm-rows")!;
    await vi.waitFor(() => expect(rows.textContent).toContain("background"));
    renderer.selectSession(session);
    await vi.waitFor(() => {
      const glyphs = [...rows.querySelectorAll<HTMLElement>("span")].filter((span) => span.textContent?.trim());
      expect(glyphs.map((span) => span.style.letterSpacing || rows.style.letterSpacing)).toEqual(glyphs.map(() => "0px"));
    });
  });
});
