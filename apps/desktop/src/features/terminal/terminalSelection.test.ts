// @vitest-environment jsdom
import { Terminal as HeadlessTerminal } from "@xterm/headless";
import type { Terminal } from "@xterm/xterm";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TerminalSelection } from "./terminalSelection";

let cleanup: (() => void) | undefined;

function setup(columns = 8, rows = 3, scrollback = 5) {
  const buffer = new HeadlessTerminal({ cols: columns, rows, scrollback, allowProposedApi: true });
  const screen = document.createElement("div");
  document.body.append(screen);
  const bounds = vi.spyOn(screen, "getBoundingClientRect").mockImplementation(() => new DOMRect(100, 200, buffer.cols * 10, buffer.rows * 20));
  const register_marker = vi.fn((offset: number) => buffer.registerMarker(offset));
  const native_selection = vi.fn(() => { throw new Error("Local selection must not use native selection APIs"); });
  const terminal = {
    get cols() { return buffer.cols; }, get rows() { return buffer.rows; },
    get buffer() { return buffer.buffer; }, options: { theme: { selectionBackground: "#446688" } },
    registerMarker: register_marker, select: native_selection, getSelection: native_selection,
  } as unknown as Terminal;
  const selection = new TerminalSelection(terminal, screen);
  const write = (text: string) => new Promise<void>((resolve) => buffer.write(text, resolve));
  const point = (column: number, row = 0) => new MouseEvent("mousemove", {
    clientX: 100 + column * 10 + 0.01, clientY: 200 + (row + 0.5) * 20,
  });
  const rectangles = () => [...screen.querySelectorAll<HTMLElement>(".terminal-selection-rect")];
  cleanup = () => { selection.dispose(); buffer.dispose(); screen.remove(); };
  return { buffer, screen, bounds, register_marker, native_selection, selection, write, point, rectangles };
}

afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  vi.restoreAllMocks();
});

describe("local terminal selection", () => {
  it("shows cell-aligned bounds only after a nonempty update and removes them on clear", async () => {
    const { selection, screen, write, point, rectangles, register_marker, native_selection } = setup();
    await write("abcdefgh");
    selection.begin(point(1));
    expect(screen.querySelector(".terminal-selection-overlay")).toBeNull();
    expect(selection.text()).toBe("");
    expect(selection.update(point(1))).toBe(true);
    expect(rectangles()).toHaveLength(0);
    expect(selection.update(point(5))).toBe(true);
    expect(selection.text()).toBe("bcde");
    const overlay = screen.querySelector<HTMLElement>(".terminal-selection-overlay")!;
    expect(overlay.style.pointerEvents).toBe("none");
    expect(overlay.style.position).toBe("absolute");
    expect(overlay.getAttribute("aria-hidden")).toBe("true");
    expect(rectangles()).toHaveLength(1);
    expect(rectangles()[0].style.left).toBe("12.5%");
    expect(rectangles()[0].style.width).toBe("50%");
    expect(rectangles()[0].style.top).toBe("0%");
    expect(parseFloat(rectangles()[0].style.height)).toBeCloseTo(100 / 3);
    expect(rectangles()[0].style.backgroundColor).toBe("rgb(68, 102, 136)");
    const marker = register_marker.mock.results[0].value;
    selection.clear();
    expect(marker.isDisposed).toBe(true);
    expect(screen.querySelector(".terminal-selection-overlay")).toBeNull();
    expect(selection.text()).toBe("");
    expect(selection.update(point(6))).toBe(false);
    expect(native_selection).not.toHaveBeenCalled();
  });

  it("copies wide and combining glyphs, joins soft wraps, and retains hard newlines", async () => {
    const { selection, write, point, rectangles } = setup(8, 4);
    await write("界e\u0301abcdEF\r\nsecond");
    selection.begin(point(0));
    selection.update(point(6, 2));
    expect(selection.text()).toBe("界e\u0301abcdEF\nsecond");
    expect(rectangles().map((rectangle) => rectangle.style.width)).toEqual(["100%", "100%", "75%"]);
    expect(rectangles().map((rectangle) => rectangle.style.top)).toEqual(["0%", "25%", "50%"]);
    selection.begin(point(0));
    selection.update(point(1));
    expect(selection.text()).toBe("界");
    expect(rectangles()[0].style.width).toBe("25%");
    selection.begin(point(3));
    selection.update(point(2));
    expect(selection.text()).toBe("e\u0301");
  });

  it("preserves printed leading and trailing spaces without copying unprinted padding", async () => {
    const { selection, write, point } = setup(10, 3);
    await write("ab  \r\n  cd ");
    selection.begin(point(0));
    selection.update(point(8, 1));
    expect(selection.text()).toBe("ab  \n  cd ");
  });

  it("sorts reverse endpoints and clamps outside the viewport to whole-row boundaries", async () => {
    const { selection, write, point, rectangles } = setup(8, 2);
    await write("first\r\nsecond");
    selection.begin(point(4, 1));
    selection.update(point(1, 0));
    expect(selection.text()).toBe("irst\nseco");
    selection.begin(point(0, 0));
    selection.update(point(2, 4));
    expect(selection.text()).toBe("first\nsecond");
    expect(rectangles().map((rectangle) => rectangle.style.width)).toEqual(["100%", "100%"]);
    selection.begin(point(6, 1));
    selection.update(point(6, -1));
    expect(selection.text()).toBe("first\nsecond");
  });

  it("uses buffer coordinates for scrollback and renders only visible selected rows", async () => {
    const { buffer, selection, write, point, rectangles } = setup(8, 2);
    await write("row 0\r\nrow 1\r\nrow 2\r\nrow 3");
    expect(buffer.buffer.active.viewportY).toBe(2);
    selection.begin(point(0));
    selection.update(point(5, 1));
    expect(selection.text()).toBe("row 2\nrow 3");
    buffer.scrollToTop();
    selection.update(point(0));
    expect(selection.text()).toBe("row 0\nrow 1\n");
    expect(rectangles()).toHaveLength(2);
    expect(rectangles().map((rectangle) => rectangle.style.top)).toEqual(["0%", "50%"]);
  });

  it("tracks the anchor through scrollback trimming and clears if its line is removed", async () => {
    const { buffer, selection, screen, write, point, register_marker } = setup(8, 2, 2);
    await write("zero\r\none\r\ntwo\r\nthree");
    selection.begin(point(0));
    const marker = register_marker.mock.results[0].value;
    expect(marker.line).toBe(2);
    await write("\r\nfour");
    expect(marker.line).toBe(1);
    expect(selection.update(point(5))).toBe(true);
    expect(selection.text()).toBe("two\nthree");
    await write("\r\nfive\r\nsix");
    expect(buffer.buffer.active.getLine(0)?.translateToString(true)).toBe("three");
    expect(marker.isDisposed).toBe(true);
    expect(selection.update(point(4))).toBe(false);
    expect(selection.text()).toBe("");
    expect(screen.querySelector(".terminal-selection-overlay")).toBeNull();
  });

  it("selects the alternate buffer without markers and clears on a buffer switch", async () => {
    const { selection, screen, write, point, register_marker } = setup();
    await write("normal\u001b[?1049h\u001b[Halt text");
    selection.begin(point(0));
    expect(selection.update(point(3))).toBe(true);
    expect(selection.text()).toBe("alt");
    expect(register_marker).not.toHaveBeenCalled();
    await write("\u001b[?1049l");
    expect(selection.update(point(3))).toBe(false);
    expect(screen.querySelector(".terminal-selection-overlay")).toBeNull();
  });

  it("removes bounds on disposal or a hidden screen without leaving a marker behind", async () => {
    const { selection, screen, bounds, write, point, register_marker } = setup();
    await write("abcdefgh");
    selection.begin(point(0));
    selection.update(point(4));
    bounds.mockReturnValue(new DOMRect());
    expect(selection.update(point(4))).toBe(false);
    expect(register_marker.mock.results[0].value.isDisposed).toBe(true);
    expect(screen.querySelector(".terminal-selection-overlay")).toBeNull();
    bounds.mockReturnValue(new DOMRect(100, 200, 80, 60));
    selection.begin(point(0));
    selection.update(point(4));
    selection.dispose();
    selection.dispose();
    expect(register_marker.mock.results[1].value.isDisposed).toBe(true);
    expect(screen.querySelector(".terminal-selection-overlay")).toBeNull();
  });
});
