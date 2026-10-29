// @vitest-environment jsdom
import type { IBufferRange, IDisposable, ITerminalOptions, Terminal } from "@xterm/xterm";
import { afterEach, describe, expect, it, vi } from "vitest";
import { installTerminalLinks } from "./terminalLinks";

// Geometry and text extraction are exercised with real buffers separately.
const selection = vi.hoisted(() => ({
  begin: vi.fn(), update: vi.fn(() => true), text: vi.fn(() => ""), clear: vi.fn(), dispose: vi.fn(),
}));
vi.mock("./terminalSelection", () => ({ TerminalSelection: class {
  begin = selection.begin;
  update = selection.update;
  text = selection.text;
  clear = selection.clear;
  dispose = selection.dispose;
} }));

const range: IBufferRange = { start: { x: 1, y: 1 }, end: { x: 19, y: 1 } };
const url = "https://example.com";
let cleanup: (() => void) | undefined;

function setup(platform = "MacIntel") {
  vi.spyOn(navigator, "platform", "get").mockReturnValue(platform);
  const element = document.createElement("div");
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  element.append(screen);
  document.body.append(element);
  vi.spyOn(screen, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 160, 32));
  const provider_dispose = vi.fn();
  const terminal = {
    element, cols: 20, rows: 2, options: {} as ITerminalOptions,
    buffer: { active: { viewportY: 0 }, onBufferChange: vi.fn(() => ({ dispose() {} })) }, focus: vi.fn(),
    modes: { mouseTrackingMode: "none" },
    onResize: vi.fn(() => ({ dispose() {} })),
    onScroll: vi.fn(() => ({ dispose() {} })),
    onWriteParsed: vi.fn(() => ({ dispose() {} })),
    clearSelection: vi.fn(), getSelection: vi.fn(() => ""),
    registerLinkProvider: vi.fn(() => ({ dispose: provider_dispose })),
    loadAddon: (addon: IDisposable & { activate(terminal: Terminal): void }) => addon.activate(terminal as unknown as Terminal),
  };
  const open = vi.fn();
  const copy = vi.fn();
  const links = installTerminalLinks(terminal as unknown as Terminal, open, copy);
  const hover = (value = url, position = range) => terminal.options.linkHandler!.hover!(new MouseEvent("mousemove"), value, position);
  const mouse = (type: string, init: MouseEventInit = {}) => {
    const event = new MouseEvent(type, { bubbles: true, cancelable: true, clientX: 12, clientY: 8, ...init });
    screen.dispatchEvent(event);
    return event;
  };
  cleanup = () => { links.dispose(); element.remove(); };
  return { element, terminal, open, copy, links, hover, mouse, provider_dispose };
}

afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  vi.useRealTimers();
  vi.restoreAllMocks();
  selection.text.mockReset().mockReturnValue("");
  vi.clearAllMocks();
});

describe("terminal link gestures", () => {
  it.each([
    ["MacIntel", "Meta", "metaKey"],
    ["Linux x86_64", "Control", "ctrlKey"],
  ])("keeps a held modifier active on %s until key-up, even without mouse modifier flags", (platform, key, flag) => {
    vi.useFakeTimers();
    const { mouse, copy } = setup(platform);
    selection.text.mockReturnValue("selected");
    window.dispatchEvent(new KeyboardEvent("keydown", { key, [flag]: true }));
    vi.advanceTimersByTime(5000);
    expect(mouse("mousedown", { buttons: 1 }).defaultPrevented).toBe(true);
    mouse("mousemove", { buttons: 1, clientX: 52 });
    window.dispatchEvent(new KeyboardEvent("keyup", { key }));
    mouse("mouseup", { clientX: 52 });
    expect(copy).toHaveBeenCalledExactlyOnceWith("selected");
    expect(mouse("mousedown", { buttons: 1 }).defaultPrevented).toBe(false);
  });

  it.each(["blur", "hidden", "keyboard"])("clears held modifier state after %s", (reset) => {
    const { mouse } = setup();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Meta", metaKey: true }));
    if (reset === "blur") {
      window.dispatchEvent(new Event("blur"));
    } else if (reset === "hidden") {
      vi.spyOn(document, "hidden", "get").mockReturnValue(true);
      document.dispatchEvent(new Event("visibilitychange"));
    } else {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "a" }));
    }
    expect(mouse("mousedown", { buttons: 1 }).defaultPrevented).toBe(false);
  });

  it("shares held modifiers across panes and forgets them when the last terminal is disposed", () => {
    const first = setup();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Meta", metaKey: true }));
    const second = setup();
    first.links.dispose();
    first.element.remove();
    expect(second.mouse("mousedown", { buttons: 1 }).defaultPrevented).toBe(true);
    cleanup?.();
    const next = setup();
    expect(next.mouse("mousedown", { buttons: 1 }).defaultPrevented).toBe(false);
  });

  it.each([
    ["MacIntel", { metaKey: true }, "Cmd"],
    ["Linux x86_64", { ctrlKey: true }, "Ctrl"],
  ])("opens a modified left click on %s and advertises the shortcut only on hover", (platform, modifiers, label) => {
    const { element, open, hover, mouse } = setup(platform);
    expect(element.title).toBe("");
    hover();
    expect(element.title).toBe(`${label}-click to open\n${url}`);
    expect(mouse("mousedown", modifiers).defaultPrevented).toBe(true);
    expect(open).not.toHaveBeenCalled();
    expect(mouse("mouseup", modifiers).defaultPrevented).toBe(true);
    expect(open).toHaveBeenCalledExactlyOnceWith(url);
    mouse("mouseup", modifiers);
    expect(open).toHaveBeenCalledOnce();
  });

  it.each([{}, { ctrlKey: true }, { metaKey: true, button: 1 }, { metaKey: true, button: 2 }])(
    "leaves ordinary and other button gestures available to xterm: %j", (modifiers) => {
      const { element, terminal, open, hover, mouse } = setup();
      const down = vi.fn();
      element.addEventListener("mousedown", down);
      hover();
      expect(mouse("mousedown", modifiers).defaultPrevented).toBe(false);
      const event = mouse("mouseup", modifiers);
      terminal.options.linkHandler!.activate(event, url, range);
      expect(down).toHaveBeenCalledOnce();
      expect(open).not.toHaveBeenCalled();
    },
  );

  it.each(["javascript:alert(1)", "file:///tmp/example", "mailto:a@example.com", "https://", "https:///example.com", "https://example.com\\other", "https://example.com\n"])(
    "does not activate an unsupported or malformed OSC 8 target: %s", (value) => {
      const { element, open, hover, mouse } = setup();
      hover(value);
      expect(element.title).toBe("");
      // Modified selection is available even when the text is not a web link.
      expect(mouse("mousedown", { metaKey: true }).defaultPrevented).toBe(true);
      mouse("mouseup", { metaKey: true });
      expect(open).not.toHaveBeenCalled();
    },
  );

  it("does not open after dragging off the link, changing links, or releasing Cmd", () => {
    const { open, hover, mouse } = setup();
    hover();
    mouse("mousedown", { metaKey: true });
    mouse("mouseup", { metaKey: true, clientX: 158 });
    hover();
    mouse("mousedown", { metaKey: true });
    hover("https://other.example");
    mouse("mouseup", { metaKey: true });
    hover();
    mouse("mousedown", { metaKey: true });
    mouse("mouseup");
    expect(open).not.toHaveBeenCalled();
  });

  it("cleans up document listeners, hover state, and the addon when disposed during a gesture", () => {
    const { element, open, links, hover, mouse, provider_dispose } = setup();
    hover();
    mouse("mousedown", { metaKey: true });
    links.dispose();
    expect(element.title).toBe("");
    expect(mouse("mouseup", { metaKey: true }).defaultPrevented).toBe(false);
    expect(open).not.toHaveBeenCalled();
    expect(provider_dispose).toHaveBeenCalledOnce();
    cleanup = () => element.remove();
  });

  it("cancels a pressed gesture on blur and opens the same hovered link after focus returns", () => {
    const { element, open, hover, mouse } = setup();
    hover();
    mouse("mousedown", { metaKey: true });
    window.dispatchEvent(new Event("blur"));
    window.dispatchEvent(new Event("focus"));
    mouse("mouseup", { metaKey: true });
    expect(open).not.toHaveBeenCalled();
    expect(element.title).toBe(`Cmd-click to open\n${url}`);
    // xterm does not emit another hover callback for the unchanged link.
    mouse("mousedown", { metaKey: true });
    mouse("mouseup", { metaKey: true });
    expect(open).toHaveBeenCalledExactlyOnceWith(url);
  });

  it("does not open when release targets another pane or an overlay at the same coordinates", () => {
    const { open, hover, mouse } = setup();
    const other = document.createElement("div");
    document.body.append(other);
    hover();
    mouse("mousedown", { metaKey: true });
    other.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true,
      clientX: 12, clientY: 8, metaKey: true }));
    mouse("mouseup", { metaKey: true });
    expect(open).not.toHaveBeenCalled();
    other.remove();
  });

  it("copies from local bounds without forwarding a drag to xterm", () => {
    const { element, terminal, mouse, open, copy } = setup();
    terminal.modes.mouseTrackingMode = "any";
    terminal.options.macOptionClickForcesSelection = false;
    terminal.options.disableStdin = false;
    selection.text.mockReturnValue("  selected text\n");
    const forwarded = vi.fn();
    for (const type of ["mousedown", "mousemove", "mouseup"]) {
      element.addEventListener(type, forwarded);
    }
    mouse("mousedown", { metaKey: true, buttons: 1 });
    expect(selection.begin).toHaveBeenCalledOnce();
    expect(selection.update).not.toHaveBeenCalled();
    mouse("mousemove", { metaKey: true, buttons: 1, clientX: 52 });
    expect(terminal.options.macOptionClickForcesSelection).toBe(false);
    expect(terminal.options.disableStdin).toBe(false);
    selection.clear.mockClear();
    copy.mockImplementation(() => { expect(selection.clear).toHaveBeenCalledOnce(); });
    mouse("mouseup", { clientX: 52 }); // Releasing Cmd early still completes the captured drag.
    expect(forwarded).not.toHaveBeenCalled();
    expect(terminal.getSelection).not.toHaveBeenCalled();
    expect(copy).toHaveBeenCalledExactlyOnceWith("  selected text\n");
    expect(open).not.toHaveBeenCalled();
  });

  it("cancels local bounds on blur and disposal without copying", () => {
    const { terminal, links, mouse, copy } = setup();
    selection.text.mockReturnValue("selected");
    mouse("mousedown", { metaKey: true, buttons: 1 });
    mouse("mousemove", { metaKey: true, buttons: 1, clientX: 52 });
    selection.clear.mockClear();
    window.dispatchEvent(new Event("blur"));
    expect(selection.clear).toHaveBeenCalledOnce();
    mouse("mouseup", { metaKey: true, clientX: 52 });
    expect(copy).not.toHaveBeenCalled();
    mouse("mousedown", { metaKey: true, buttons: 1 });
    mouse("mousemove", { metaKey: true, buttons: 1, clientX: 52 });
    links.dispose();
    expect(selection.dispose).toHaveBeenCalledOnce();
    mouse("mouseup", { metaKey: true, clientX: 52 });
    expect(copy).not.toHaveBeenCalled();
    cleanup = () => terminal.element.remove();
  });
});
