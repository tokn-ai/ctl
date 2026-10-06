// @vitest-environment jsdom
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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
  renderer = new XtermRenderer(container, () => {}, size);
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
