// @vitest-environment jsdom
import { afterAll, afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Mock } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Terminal } from "@xterm/xterm";

const original_get_context = vi.hoisted(() => {
  const original = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = (() => null) as typeof HTMLCanvasElement.prototype.getContext;
  return original;
});

afterAll(() => { HTMLCanvasElement.prototype.getContext = original_get_context; });

let pane: HTMLElement;
let styles: HTMLStyleElement;
let terminal: Terminal;
let on_input: Mock<(data: string) => void>;

beforeEach(() => {
  vi.stubGlobal("OffscreenCanvas", class {
    getContext() {
      return { font: "", measureText: (text: string) => ({ width: text.length * 8, fontBoundingBoxAscent: 11, fontBoundingBoxDescent: 2 }) };
    }
  });
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => setTimeout(() => callback(performance.now()), 0));
  vi.stubGlobal("cancelAnimationFrame", (id: number) => clearTimeout(id));
  vi.stubGlobal("matchMedia", () => ({ matches: false, addListener() {}, removeListener() {} }));
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("xterm-char-measure-element") ? (this.textContent?.length ?? 0) * 8 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(13);
  styles = document.createElement("style");
  // Vitest stubs CSS imports; apply the shipped styles to the real xterm DOM.
  const source_dir = dirname(fileURLToPath(import.meta.url));
  styles.textContent = readFileSync(resolve(source_dir, "../../../node_modules/@xterm/xterm/css/xterm.css"), "utf8") +
    readFileSync(resolve(source_dir, "sessionView.css"), "utf8");
  document.head.append(styles);
  pane = document.createElement("div");
  pane.className = "view-pane";
  document.body.append(pane);
  terminal = new Terminal({ cols: 20, rows: 6, scrollback: 100, fontSize: 13, lineHeight: 1.18 });
  terminal.open(pane);
  on_input = vi.fn();
  terminal.onData(on_input);
});

afterEach(() => {
  terminal.dispose();
  pane.remove();
  styles.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function bufferLines(): string[] {
  const buffer = terminal.buffer.active;
  return Array.from({ length: buffer.length }, (_, index) => buffer.getLine(index)?.translateToString(true) ?? "");
}

function expectVisibleRows(): void {
  const buffer = terminal.buffer.active;
  const rows = [...pane.querySelectorAll(".xterm-rows > div")];
  expect(rows).toHaveLength(terminal.rows);
  expect(rows.map((row) => row.textContent?.trimEnd())).toEqual(
    Array.from({ length: terminal.rows }, (_, index) => buffer.getLine(buffer.viewportY + index)?.translateToString(true) ?? ""),
  );
}

it("hides the xterm 6 pane overlay after resizing while wheel scrolling retains correct rows", async () => {
  await new Promise<void>((resolve) => terminal.write(Array.from({ length: 30 }, (_, index) => `row ${index}`).join("\r\n"), resolve));
  terminal.resize(18, 4);
  await vi.waitFor(expectVisibleRows);
  const scrollbar = pane.querySelector<HTMLElement>(".xterm-scrollable-element > .scrollbar.vertical")!;
  expect(scrollbar).not.toBeNull();
  expect(terminal.buffer.active.baseY).toBeGreaterThan(0);
  expect(getComputedStyle(scrollbar).display).toBe("none");
  const lines = bufferLines();
  const initial_viewport = terminal.buffer.active.viewportY;
  const screen = pane.querySelector<HTMLElement>(".xterm-screen")!;
  screen.dispatchEvent(new WheelEvent("wheel", { bubbles: true, cancelable: true, deltaY: -120 }));
  await vi.waitFor(() => {
    expect(terminal.buffer.active.viewportY).toBeLessThan(initial_viewport);
    expectVisibleRows();
  });
  screen.dispatchEvent(new WheelEvent("wheel", { bubbles: true, cancelable: true, deltaY: 1200 }));
  await vi.waitFor(() => {
    expect(terminal.buffer.active.viewportY).toBe(terminal.buffer.active.baseY);
    expectVisibleRows();
  });
  expect(bufferLines()).toEqual(lines);
  expect(on_input).not.toHaveBeenCalled();
});

it("keeps the scrollbar policy scoped to session panes", () => {
  const scrollbar = pane.querySelector<HTMLElement>(".xterm-scrollable-element > .scrollbar.vertical")!;
  expect(getComputedStyle(scrollbar).display).toBe("none");
  pane.classList.remove("view-pane");
  expect(getComputedStyle(scrollbar).display).not.toBe("none");
});
