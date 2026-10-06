// @vitest-environment jsdom
import { useEffect, useRef, useState } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DividerResize, SessionSummary, SessionView } from "../../lib/types";
import { publishLayoutOwnerChange, publishPaneResizeResult, publishSessionView, registerAttachmentControl, subscribeSessionViews } from "../../features/attachment/componentActions";
import { ViewDividers } from "./ViewDividers";

class TestPointerEvent extends MouseEvent {
  readonly pointerId: number;
  readonly isPrimary: boolean;
  constructor(type: string, options: PointerEventInit = {}) {
    super(type, options);
    this.pointerId = options.pointerId ?? 1;
    this.isPrimary = options.isPrimary ?? true;
  }
}

const size = { columns: 80, rows: 24, pixel_width: null, pixel_height: null };
const session: SessionSummary = { target: { kind: "local" }, session_id: "root", terminal_id: "a", name: "Root", status: "running", next_sequence: "0", terminal_size: size };
const leaf = (terminal_id: string) => ({ kind: "terminal" as const, terminal_id });
const initial: SessionView = {
  session_id: "root", session_name: "Root", view_id: "view", revision: "1", canvas_size: size, zoomed_terminal_id: null,
  layout: { kind: "split", axis: "horizontal", children: [leaf("a"), leaf("b")] },
  panes: [{ terminal_id: "a", left: 0, top: 0, columns: 40, rows: 24 }, { terminal_id: "b", left: 41, top: 0, columns: 39, rows: 24 }],
  terminals: ["a", "b"].map((terminal_id) => ({ terminal_id, name: terminal_id, terminal_size: size, next_sequence: "0" })),
};
const resized = (position = 45, revision = "2"): SessionView => ({ ...initial, revision,
  layout: { kind: "split", axis: "horizontal", children: [leaf("a"), leaf("b")], weights: [position, 79 - position] },
  panes: [{ ...initial.panes[0], columns: position }, { ...initial.panes[1], left: position + 1, columns: 79 - position }],
});
const resize = vi.fn(async (_divider: DividerResize, _request_id: string) => {});
const confirm = vi.fn();
const errors = vi.fn();
const stops: (() => void)[] = [];
let owned = true;
let captured: WeakMap<HTMLElement, number>;
let frames: Map<number, FrameRequestCallback>;
let next_frame: number;

function Fixture({ view = initial, cell = { width: 8, height: 16 }, enabled = true, current_session = session }: {
  view?: SessionView; cell?: { width: number; height: number }; enabled?: boolean; current_session?: SessionSummary;
}) {
  const [current, setCurrent] = useState(view);
  const [busy, setBusy] = useState(false);
  const busy_ref = useRef(false);
  useEffect(() => { setCurrent(view); }, [view]);
  useEffect(() => subscribeSessionViews((event) => {
    if (event.view?.session_id === current_session.session_id) setCurrent((previous) =>
      previous.view_id !== event.view!.view_id || BigInt(event.view!.revision) >= BigInt(previous.revision) ? event.view! : previous);
  }), [current_session]);
  return <div>
    <output data-testid="busy">{String(busy)}</output>
    <output data-testid="geometry">{current.panes[0].columns}</output>
    <ViewDividers session={current_session} view={current} cell={cell} enabled={enabled} can_begin={() => !busy_ref.current}
      on_busy={(next) => { busy_ref.current = next; setBusy(next); }} on_error={errors}
      on_confirm={(next) => { confirm(next); setCurrent(next); }} />
  </div>;
}

function down(handle = screen.getByRole("separator"), x = 324, y = 100) {
  fireEvent.pointerDown(handle, { pointerId: 1, button: 0, clientX: x, clientY: y });
  return handle;
}
function paint() {
  const ready = [...frames];
  frames.clear();
  act(() => { for (const [, callback] of ready) callback(0); });
}
function move(handle: HTMLElement, x: number, y = 100, paint_now = true) {
  fireEvent.pointerMove(handle, { pointerId: 1, clientX: x, clientY: y });
  if (paint_now) paint();
}
async function acknowledge(view: SessionView, index = resize.mock.calls.length - 1) {
  await act(async () => publishPaneResizeResult({ session, attachment_id: "owner", request_id: resize.mock.calls[index][1], view, error: null }));
}

beforeEach(() => {
  owned = true;
  captured = new WeakMap();
  frames = new Map();
  next_frame = 0;
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    const id = ++next_frame;
    frames.set(id, callback);
    return id;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
  vi.stubGlobal("PointerEvent", TestPointerEvent);
  Object.defineProperty(HTMLElement.prototype, "setPointerCapture", { configurable: true, value(this: HTMLElement, pointer_id: number) { captured.set(this, pointer_id); } });
  Object.defineProperty(HTMLElement.prototype, "hasPointerCapture", { configurable: true, value(this: HTMLElement, pointer_id: number) { return captured.get(this) === pointer_id; } });
  Object.defineProperty(HTMLElement.prototype, "releasePointerCapture", { configurable: true, value(this: HTMLElement) { captured.delete(this); } });
  stops.push(registerAttachmentControl({ attachmentId: () => "owner", session: () => session, layoutOwned: () => owned,
    layoutLease: () => ({ held: owned, owned_by_client: owned }), requestResizeControl: async () => {},
    resizeWithWindow: () => false, toggleResizeWithWindow: async () => {},
    enqueueViewportResize: () => {}, proposeViewportSize: () => null,
    resizeDivider: resize, resizePane: async () => {}, setViewZoom: async () => {}, reconnect: async () => null, reset: () => {} }));
});
afterEach(() => {
  cleanup(); for (const stop of stops.splice(0)) stop();
  for (const method of ["setPointerCapture", "hasPointerCapture", "releasePointerCapture"]) Reflect.deleteProperty(HTMLElement.prototype, method);
  vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.clearAllMocks();
});

describe("GUI divider dragging", () => {
  it("coalesces pointer moves and preview updates into one target per paint", async () => {
    render(<Fixture />);
    const handle = down();
    move(handle, 332, 100, false);
    move(handle, 340, 100, false);
    move(handle, 348, 100, false);
    expect(resize).not.toHaveBeenCalled();
    expect(document.querySelector(".view-divider-preview")).toBeNull();
    expect(frames.size).toBe(1);
    paint();
    expect(resize).toHaveBeenCalledTimes(1);
    expect(resize.mock.calls[0][0]).toMatchObject({ position: 43 });
    expect(document.querySelector<HTMLElement>(".view-divider-preview")!.style.left).toBe("348px");
    move(handle, 356, 100, false);
    move(handle, 364, 100, false);
    paint();
    expect(resize).toHaveBeenCalledTimes(1);
    await acknowledge(resized(43));
    expect(resize).toHaveBeenCalledTimes(2);
    expect(resize.mock.calls[1][0]).toMatchObject({ position: 45, expected_revision: "2" });
    await acknowledge(resized(45, "3"));
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: 364, clientY: 100 });
    expect(screen.getByTestId("busy").textContent).toBe("false");
  });

  it("waits for the next paint when an acknowledgement arrives before queued pointer motion", async () => {
    render(<Fixture />);
    const handle = down();
    move(handle, 332);
    move(handle, 348, 100, false);
    await acknowledge(resized(41));
    expect(resize).toHaveBeenCalledTimes(1);
    expect(frames.size).toBe(1);
    paint();
    expect(resize).toHaveBeenCalledTimes(2);
    expect(resize.mock.calls[1][0]).toMatchObject({ position: 43, expected_revision: "2" });
    await acknowledge(resized(43, "3"));
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: 348, clientY: 100 });
    expect(screen.getByTestId("busy").textContent).toBe("false");
  });

  it("flushes mouseup before the next paint without losing its final target", async () => {
    render(<Fixture />);
    const handle = down();
    move(handle, 332, 100, false);
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: 348, clientY: 100 });
    fireEvent.lostPointerCapture(handle, { pointerId: 1 });
    expect(frames.size).toBe(0);
    expect(captured.has(handle)).toBe(false);
    expect(resize).toHaveBeenCalledTimes(1);
    expect(resize.mock.calls[0][0]).toMatchObject({ position: 43 });
    expect(screen.getByTestId("busy").textContent).toBe("true");
    await acknowledge(resized(43));
    expect(screen.getByTestId("busy").textContent).toBe("false");
    paint();
    expect(resize).toHaveBeenCalledTimes(1);
  });

  it.each(["escape", "unmount", "lease"])("discards an unsent frame on %s and fences cancelled callbacks", async (reason) => {
    const mounted = render(<Fixture />);
    const handle = down();
    move(handle, 332, 100, false);
    const callback = [...frames.values()][0];
    if (reason === "escape") fireEvent.keyDown(window, { key: "Escape" });
    if (reason === "unmount") mounted.unmount();
    if (reason === "lease") { owned = false; await act(async () => publishLayoutOwnerChange()); }
    expect(frames.size).toBe(0);
    act(() => callback(0));
    expect(resize).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
    if (reason !== "unmount") expect(screen.getByTestId("busy").textContent).toBe("false");
  });

  it("captures the pointer, snaps without a grab jump, and renders only confirmed geometry", async () => {
    render(<Fixture />);
    const handle = down(undefined, 321);
    expect(handle.style.width).toBe("8px");
    expect(captured.get(handle)).toBe(1);
    move(handle, 324);
    expect(resize).not.toHaveBeenCalled();
    move(handle, 362);
    expect(resize).toHaveBeenCalledExactlyOnceWith({ view_id: "view", expected_revision: "1", split_path: [], boundary: 0, position: 45 }, expect.any(String));
    expect(screen.getByTestId("geometry").textContent).toBe("40");
    await acknowledge(resized(45));
    expect(screen.getByTestId("geometry").textContent).toBe("45");
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: 362, clientY: 100 });
    expect(captured.has(handle)).toBe(false);
    expect(screen.getByTestId("busy").textContent).toBe("false");
  });

  it("targets an outer divider exactly when a child split has the same axis", async () => {
    const nested: SessionView = { ...initial, layout: { kind: "split", axis: "horizontal", children: [
      { kind: "split", axis: "horizontal", children: [leaf("a"), leaf("b")] }, leaf("c"),
    ] }, panes: [{ ...initial.panes[0], columns: 19 }, { ...initial.panes[1], left: 20, columns: 20 },
      { ...initial.panes[1], terminal_id: "c", left: 41, columns: 39 }] };
    render(<Fixture view={nested} />);
    const outer = document.querySelector<HTMLElement>('[data-divider-path="root.0"]')!;
    down(outer);
    move(outer, 332);
    expect(resize.mock.calls[0][0]).toEqual({ view_id: "view", expected_revision: "1", split_path: [], boundary: 0, position: 41 });
    await acknowledge({ ...nested, revision: "2" });
    fireEvent.pointerCancel(outer, { pointerId: 1 });
    const inner = document.querySelector<HTMLElement>('[data-divider-path="root.0.0"]')!;
    down(inner, 156);
    move(inner, 164);
    expect(resize.mock.calls[1][0]).toMatchObject({ split_path: [0], boundary: 0, position: 20, expected_revision: "2" });
    await acknowledge({ ...nested, revision: "3" });
  });

  it("coalesces pending targets, accepts own weight broadcasts, and keeps the final mouseup target after capture release", async () => {
    render(<Fixture />);
    const handle = down();
    move(handle, 332);
    move(handle, 340);
    move(handle, 348);
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: 356, clientY: 100 });
    fireEvent.lostPointerCapture(handle, { pointerId: 1 });
    expect(resize).toHaveBeenCalledTimes(1);
    await act(async () => publishSessionView({ session, attachment_id: "observer", view: resized(41) }));
    expect(screen.getByTestId("busy").textContent).toBe("true");
    await acknowledge(resized(41));
    expect(resize).toHaveBeenCalledTimes(2);
    expect(resize.mock.calls[1][0]).toMatchObject({ expected_revision: "2", position: 44 });
    await acknowledge(resized(44, "3"));
    expect(screen.getByTestId("busy").textContent).toBe("false");
    expect(screen.getByTestId("geometry").textContent).toBe("44");
  });

  it("drops pending work when another mutation overtakes the exact result", async () => {
    render(<Fixture />);
    const handle = down();
    move(handle, 332);
    move(handle, 340);
    await act(async () => publishSessionView({ session, attachment_id: "observer", view: resized(60, "9") }));
    await acknowledge(resized(41));
    expect(resize).toHaveBeenCalledTimes(1);
    expect(confirm).not.toHaveBeenCalled();
    expect(screen.getByTestId("geometry").textContent).toBe("60");
    expect(screen.getByTestId("busy").textContent).toBe("false");
  });

  it("uses backend minimum geometry instead of the pointer preview", async () => {
    render(<Fixture />);
    const handle = down();
    move(handle, -1000);
    expect(resize.mock.calls[0][0].position).toBe(0);
    await acknowledge(resized(3));
    expect(screen.getByTestId("geometry").textContent).toBe("3");
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: -1000, clientY: 100 });
    expect(resize).toHaveBeenCalledTimes(1);
  });

  it.each(["escape", "cancel", "capture", "blur"])("cancels %s before an ACK, preserves published geometry and prevents a second in-flight gesture", async (reason) => {
    render(<Fixture />);
    const handle = down();
    move(handle, 332);
    move(handle, 340);
    if (reason === "escape") fireEvent.keyDown(window, { key: "Escape" });
    if (reason === "cancel") fireEvent.pointerCancel(handle, { pointerId: 1 });
    if (reason === "capture") fireEvent.lostPointerCapture(handle, { pointerId: 1 });
    if (reason === "blur") fireEvent.blur(window);
    expect(screen.getByTestId("busy").textContent).toBe("true");
    down(handle); move(handle, 348);
    expect(resize).toHaveBeenCalledTimes(1);
    await acknowledge(resized(41));
    expect(confirm).not.toHaveBeenCalled();
    expect(screen.getByTestId("geometry").textContent).toBe("41");
    down(handle); move(handle, 332);
    expect(resize).toHaveBeenCalledTimes(2);
    expect(resize.mock.calls[1][0].expected_revision).toBe("2");
    await acknowledge(resized(42, "3"));
  });

  it.each(["view", "topology", "canvas", "font", "zoom", "disabled", "session", "lease"])("cancels a gesture on %s changes without applying stale callbacks", async (change) => {
    const mounted = render(<Fixture />);
    const handle = down(); move(handle, 332); move(handle, 340);
    if (change === "lease") { owned = false; await act(async () => publishLayoutOwnerChange()); }
    else mounted.rerender(<Fixture
      current_session={change === "session" ? { ...session, session_id: "other" } : session}
      cell={change === "font" ? { width: 9, height: 18 } : undefined} enabled={change !== "disabled"}
      view={change === "view" ? { ...initial, view_id: "replacement" }
        : change === "topology" ? { ...initial, layout: { kind: "split", axis: "vertical", children: [leaf("a"), leaf("b")] } }
        : change === "canvas" ? { ...initial, canvas_size: { ...size, columns: 100 } }
        : change === "zoom" ? { ...initial, zoomed_terminal_id: "a" } : initial} />);
    expect(screen.getByTestId("busy").textContent).toBe("true");
    await acknowledge(resized(41));
    expect(confirm).not.toHaveBeenCalled();
    expect(resize).toHaveBeenCalledTimes(1);
  });

  it("reports ownership and capability errors without taking a lease", async () => {
    owned = false;
    render(<Fixture />);
    const handle = down();
    expect(handle.getAttribute("aria-disabled")).toBe("true");
    expect(errors).toHaveBeenLastCalledWith("Take resize control to resize panes.");
    expect(resize).not.toHaveBeenCalled();
    owned = true; await act(async () => publishLayoutOwnerChange());
    resize.mockRejectedValueOnce(new Error("This server does not support pane resizing."));
    down(handle); move(handle, 332);
    await waitFor(() => expect(errors).toHaveBeenLastCalledWith("This server does not support pane resizing."));
    expect(screen.getByTestId("busy").textContent).toBe("false");
  });

  it("supports keyboard adjustment of a focused separator and confirms minimum no-ops", async () => {
    render(<Fixture />);
    const handle = screen.getByRole("separator");
    fireEvent.keyDown(handle, { key: "ArrowRight", altKey: true });
    expect(resize.mock.calls[0][0].position).toBe(45);
    await acknowledge(initial);
    expect(screen.getByTestId("busy").textContent).toBe("false");
    fireEvent.keyDown(handle, { key: "ArrowDown" });
    expect(resize).toHaveBeenCalledTimes(1);
  });
});
