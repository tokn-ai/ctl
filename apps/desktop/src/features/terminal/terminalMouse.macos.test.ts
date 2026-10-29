// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { IDisposable, Terminal } from "@xterm/xterm";
import { installTerminalMouse } from "./terminalMouse";

const browser_bundle = readFileSync(createRequire(import.meta.url).resolve("@xterm/xterm"), "utf8");
let terminal: Terminal;
let bridge: IDisposable;
let container: HTMLElement;
let input: string[];
let copy: ReturnType<typeof vi.fn<(text: string) => void>>;

beforeEach(() => {
  vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("OffscreenCanvas", class {
    getContext() {
      return { font: "", measureText: (text: string) => ({ width: text.length * 8, fontBoundingBoxAscent: 11, fontBoundingBoxDescent: 2 }) };
    }
  });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {} }));
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("xterm-char-measure-element") ? (this.textContent?.length ?? 0) * 8 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(13);
  // Evaluate the shipped browser bundle with Node bindings excluded.
  // Importing xterm normally makes its static Platform.isNode true in Vitest,
  // even after navigator.platform is changed to MacIntel.
  const BrowserTerminal = window.eval(`(function(process, exports, module) {
    if (typeof process !== "undefined") throw new Error("Browser bundle must not see Node process");
    ${browser_bundle}
    const Constructor = globalThis.Terminal;
    delete globalThis.Terminal;
    return Constructor;
  })(undefined, undefined, undefined)`) as typeof Terminal;
  terminal = new BrowserTerminal({ cols: 20, rows: 2, fontSize: 13, lineHeight: 1.18 });
  container = document.createElement("div");
  document.body.append(container);
  terminal.open(container);
  const screen = terminal.element!.querySelector<HTMLElement>(".xterm-screen")!;
  const width = parseFloat(screen.style.width);
  const height = parseFloat(screen.style.height);
  for (const element of [terminal.element!, screen, terminal.element!.querySelector<HTMLElement>(".xterm-viewport")!]) {
    element.style.padding = "0px";
    vi.spyOn(element, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, width, height));
  }
  input = [];
  copy = vi.fn();
  terminal.onData((data) => input.push(data));
  terminal.onBinary((data) => input.push(data));
  bridge = installTerminalMouse(terminal, true, { link_at: () => null, on_open_link: vi.fn(), on_copy_text: copy });
});

afterEach(() => {
  bridge?.dispose();
  terminal?.reset();
  terminal?.dispose();
  container?.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function write(text: string): Promise<void> {
  return new Promise((resolve) => terminal.write(text, resolve));
}

function mouse(target: string, type: string, column: number, init: MouseEventInit = {}, row = 1) {
  const screen = terminal.element!.querySelector<HTMLElement>(".xterm-screen")!;
  const rect = screen.getBoundingClientRect();
  terminal.element!.querySelector(target)!.dispatchEvent(new MouseEvent(type, {
    bubbles: true, cancelable: true, detail: 1,
    clientX: (column - 0.5) * rect.width / terminal.cols, clientY: (row - 0.5) * rect.height / terminal.rows,
    ...init,
  }));
}

function expectLocalSelection() {
  expect(terminal.element!.querySelector(".terminal-selection-overlay")).not.toBeNull();
  expect(terminal.element!.querySelectorAll(".terminal-selection-rect").length).toBeGreaterThan(0);
  expect(terminal.hasSelection()).toBe(false);
}

function expectNoSelection() {
  expect(terminal.element!.querySelector(".terminal-selection-overlay")).toBeNull();
  expect(terminal.hasSelection()).toBe(false);
}

function commandKey(type: "keydown" | "keyup") {
  terminal.textarea!.dispatchEvent(new KeyboardEvent(type, {
    bubbles: true, cancelable: true, key: "Meta", code: "MetaLeft", keyCode: 91,
    metaKey: type === "keydown",
  }));
}

async function holdCommand() {
  terminal.focus();
  commandKey("keydown");
  await new Promise((resolve) => setTimeout(resolve, 350));
}

describe("macOS browser terminal mouse integration", () => {
  it("uses xterm's actual macOS Option-selection policy instead of its Node policy", async () => {
    await write("alpha beta gamma\u001b[?1000h\u001b[?1006h");
    terminal.options.macOptionClickForcesSelection = true;
    mouse(".xterm-screen", "mousedown", 1, { shiftKey: true, buttons: 1 });
    mouse(".xterm-screen", "mouseup", 1, { shiftKey: true });
    expect(input).not.toHaveLength(0);
    input.length = 0;
    mouse(".xterm-screen", "mousedown", 1, { altKey: true, buttons: 1 });
    mouse(".xterm-screen", "mousemove", 11, { altKey: true, buttons: 1 });
    mouse(".xterm-screen", "mouseup", 11, { altKey: true });
    expect(terminal.getSelection()).toBe("alpha beta");
    expect(input).toEqual([]);
  });

  it.each([".xterm-screen", ".xterm-viewport"].flatMap((target) => [9, 1000, 1002, 1003].map((mode) => ({ target, mode }))))(
    "owns Cmd-drag targeting $target in mouse mode $mode", async ({ target, mode }) => {
      await write(`alpha beta gamma\u001b[?${mode}h\u001b[?1006h`);
      mouse(target, "mousemove", 1, { metaKey: true });
      mouse(target, "mousedown", 1, { metaKey: true, buttons: 1 });
      expectNoSelection();
      mouse(target, "mousemove", 11, { metaKey: true, buttons: 1 });
      expectLocalSelection();
      mouse(target, "mouseup", 11, { metaKey: true });
      mouse(target, "mousemove", 12, { metaKey: true });
      expect(input).toEqual([]);
      expectNoSelection();
      expect(copy).toHaveBeenCalledExactlyOnceWith("alpha beta");
    },
  );

  it.each(["root", "right gutter", "bottom gutter"].flatMap((target) => [null, 9, 1000, 1002, 1003].map((mode) => ({ target, mode }))))(
    "owns a delayed Cmd-drag starting on the $target in mouse mode $mode", async ({ target, mode }) => {
      await write(`alpha beta gamma\r\nsecond row${mode === null ? "" : `\u001b[?${mode}h\u001b[?1006h`}`);
      await holdCommand();
      const root = terminal.element!;
      const screen = root.querySelector<HTMLElement>(".xterm-screen")!;
      const rect = screen.getBoundingClientRect();
      // The root fills its pane while the screen occupies an integral cell
      // grid. Hits in the remaining pixels still reach xterm's root handlers.
      vi.mocked(root.getBoundingClientRect).mockReturnValue(new DOMRect(0, 0, rect.width + 8, rect.height + 8));
      const start = target === "right gutter" ? { clientX: rect.right + 4, clientY: rect.height / 4 }
        : target === "bottom gutter" ? { clientX: rect.width / 2, clientY: rect.bottom + 4 }
          : { clientX: rect.width / terminal.cols / 2, clientY: rect.height / 4 };
      root.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, detail: 1, buttons: 1, metaKey: true, ...start }));
      // A captured gesture remains local even when Cmd is released mid-drag.
      commandKey("keyup");
      mouse(".xterm-screen", "mousemove", target === "root" ? 11 : 1, { buttons: 1 });
      const preview = root.querySelector(".terminal-selection-overlay");
      const native_selection = terminal.hasSelection();
      mouse(".xterm-screen", "mouseup", target === "root" ? 11 : 1);
      expect.soft(input).toEqual([]);
      expect.soft(native_selection).toBe(false);
      expect.soft(preview).not.toBeNull();
      expect(copy).toHaveBeenCalledExactlyOnceWith(target === "root" ? "alpha beta" : target === "right gutter" ? "alpha beta gamma" : "alpha beta gamma\nsecond row");
      expectNoSelection();
    },
  );

  it.each(["normal", "alternate"])("uses linear multirow selection in the macOS %s buffer", async (buffer) => {
    await write(`${buffer === "alternate" ? "\u001b[?1049h" : ""}alpha beta\r\nsecond row\u001b[?1003h\u001b[?1006h`);
    mouse(".xterm-screen", "mousedown", 1, { metaKey: true, buttons: 1 });
    mouse(".xterm-screen", "mousemove", 7, { metaKey: true, buttons: 1 }, 2);
    expectLocalSelection();
    mouse(".xterm-screen", "mouseup", 7, { metaKey: true }, 2);
    expect(input).toEqual([]);
    expectNoSelection();
    expect(copy).toHaveBeenCalledExactlyOnceWith("alpha beta\nsecond");
  });

  it.each([null, 1003])("owns gutter movement while Cmd stays held in mouse mode %s", async (mode) => {
    await write(`alpha beta gamma\r\nsecond row${mode === null ? "" : "\u001b[?1003h\u001b[?1006h"}`);
    await holdCommand();
    const root = terminal.element!;
    const rect = root.querySelector<HTMLElement>(".xterm-screen")!.getBoundingClientRect();
    vi.mocked(root.getBoundingClientRect).mockReturnValue(new DOMRect(0, 0, rect.width + 8, rect.height + 8));
    root.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, detail: 1, buttons: 1, metaKey: true,
      clientX: rect.right + 4, clientY: rect.height / 4 }));
    const finish = { bubbles: true, cancelable: true, metaKey: true, clientX: rect.width / 2, clientY: rect.bottom + 4 };
    root.dispatchEvent(new MouseEvent("mousemove", { ...finish, buttons: 1 }));
    const preview = root.querySelector(".terminal-selection-overlay");
    const native_selection = terminal.hasSelection();
    root.dispatchEvent(new MouseEvent("mouseup", finish));
    commandKey("keyup");
    expect.soft(input).toEqual([]);
    expect.soft(native_selection).toBe(false);
    expect.soft(preview).not.toBeNull();
    expect(copy).toHaveBeenCalledExactlyOnceWith("\nsecond row");
    expectNoSelection();
  });

  it("owns Cmd-drag without xterm selection APIs or synthetic drag events", async () => {
    await write("alpha beta gamma\u001b[?1003h\u001b[?1006h");
    const read = vi.spyOn(terminal, "getSelection");
    const select = vi.spyOn(terminal, "select");
    const delivered = vi.fn();
    for (const type of ["mousedown", "mousemove", "mouseup"]) terminal.element!.addEventListener(type, delivered);
    mouse(".xterm-viewport", "mousedown", 1, { metaKey: true, buttons: 1 });
    expectNoSelection();
    mouse(".xterm-viewport", "mousemove", 11, { metaKey: true, buttons: 1 });
    expectLocalSelection();
    mouse(".xterm-viewport", "mouseup", 11, { metaKey: true });
    expectNoSelection();
    expect(copy).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expect(delivered).not.toHaveBeenCalled();
    expect(read).not.toHaveBeenCalled();
    expect(select).not.toHaveBeenCalled();
    expect(input).toEqual([]);
  });

  it("keeps a delayed Cmd hold local when the mouse flags remain intact", async () => {
    await write("alpha beta gamma\u001b[?1003h\u001b[?1006h");
    await holdCommand();
    mouse(".xterm-viewport", "mousemove", 1, { metaKey: true });
    await new Promise((resolve) => setTimeout(resolve, 350));
    mouse(".xterm-viewport", "mousedown", 1, { metaKey: true, buttons: 1 });
    mouse(".xterm-viewport", "mousemove", 11, { metaKey: true, buttons: 1 });
    expectLocalSelection();
    mouse(".xterm-viewport", "mouseup", 11, { metaKey: true });
    commandKey("keyup");
    expect(copy).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expectNoSelection();
    expect(input).toEqual([]);
  });

  it.each([null, 1003])("recognizes a held Cmd key when later mouse flags are absent in mode %s", async (mode) => {
    await write(`alpha beta gamma${mode === null ? "" : "\u001b[?1003h\u001b[?1006h"}`);
    await holdCommand();
    // Simulate missing modifier flags without asserting that a browser emits
    // them: the observed keydown still establishes that Cmd has not been released.
    mouse(".xterm-viewport", "mousemove", 1);
    mouse(".xterm-viewport", "mousedown", 1, { buttons: 1 });
    mouse(".xterm-viewport", "mousemove", 11, { buttons: 1 });
    const preview = terminal.element!.querySelector(".terminal-selection-overlay");
    const native_selection = terminal.hasSelection();
    mouse(".xterm-viewport", "mouseup", 11);
    commandKey("keyup");
    expect(preview).not.toBeNull();
    expect(native_selection).toBe(false);
    expect(copy).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expectNoSelection();
    expect(input).toEqual([]);
  });

  it("finishes a captured drag after Cmd is released", async () => {
    await write("alpha beta gamma\u001b[?1003h\u001b[?1006h");
    commandKey("keydown");
    mouse(".xterm-viewport", "mousedown", 1, { metaKey: true, buttons: 1 });
    mouse(".xterm-viewport", "mousemove", 6, { metaKey: true, buttons: 1 });
    expectLocalSelection();
    commandKey("keyup");
    mouse(".xterm-viewport", "mousemove", 11, { buttons: 1 });
    mouse(".xterm-viewport", "mouseup", 11);
    expect(copy).toHaveBeenCalledExactlyOnceWith("alpha beta");
    expectNoSelection();
    expect(input).toEqual([]);
  });

  it.each(["keyup", "blur"])("restores ordinary native selection after %s clears held Cmd state", async (transition) => {
    await write("alpha beta gamma");
    await holdCommand();
    if (transition === "keyup") commandKey("keyup");
    else window.dispatchEvent(new Event("blur"));
    mouse(".xterm-screen", "mousedown", 1, { buttons: 1 });
    mouse(".xterm-screen", "mousemove", 11, { buttons: 1 });
    mouse(".xterm-screen", "mouseup", 11);
    expect(terminal.getSelection()).toBe("alpha beta");
    expect(terminal.element!.querySelector(".terminal-selection-overlay")).toBeNull();
    expect(copy).not.toHaveBeenCalled();
    expect(input).toEqual([]);
  });
});
