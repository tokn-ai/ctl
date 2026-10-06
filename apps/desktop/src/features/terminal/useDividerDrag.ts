import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { KeyboardEvent, PointerEvent } from "react";
import type { SessionSummary, SessionView } from "../../lib/types";
import { errorMessage } from "../../lib/errors";
import { resizeSessionDivider, sessionLayoutOwned, subscribeLayoutOwners } from "../attachment/componentActions";
import { sessionKey } from "../targets/targets";
import { layoutTopology, type ViewDivider } from "./viewLayout";

interface Options {
  session: SessionSummary | null;
  view: SessionView | null;
  cell: { width: number; height: number };
  enabled: boolean;
  current_view?(): SessionView | null;
  can_begin(): boolean;
  on_busy(busy: boolean): void;
  on_error(message: string | null): void;
  on_confirm(view: SessionView): void;
}

interface Drag {
  divider: ViewDivider;
  baseline: SessionView;
  session: SessionSummary;
  signature: string;
  element: HTMLElement;
  pointer_id: number | null;
  coordinate: number;
  initial_position: number;
  latest_position: number;
  pending: number | null;
  running: boolean;
}

function currentView(options: Options): SessionView | null {
  return options.current_view ? options.current_view() : options.view;
}

function signature(options: Options): string {
  const { session, cell, enabled } = options;
  const view = currentView(options);
  return JSON.stringify([session && sessionKey(session), view?.view_id, view && layoutTopology(view.layout),
    view?.canvas_size.columns, view?.canvas_size.rows, view?.zoomed_terminal_id, cell.width, cell.height, enabled]);
}

/** Capture one divider interaction and send only the latest unsent grid target. */
export function useDividerDrag(options: Options) {
  const latest = useRef(options);
  latest.current = options;
  const drag = useRef<Drag | null>(null);
  // Cancellation hides feedback immediately, but the wire operation still
  // needs its exact acknowledgement before another gesture can use its view.
  const in_flight = useRef<Drag | null>(null);
  const [preview, setPreview] = useState<{ path: string; position: number } | null>(null);
  const owned = useSyncExternalStore(subscribeLayoutOwners, () => sessionLayoutOwned(options.session), () => false);
  const identity = signature(options);

  function release(worker: Drag) {
    const pointer_id = worker.pointer_id;
    worker.pointer_id = null;
    if (pointer_id !== null && worker.element.hasPointerCapture?.(pointer_id)) {
      worker.element.releasePointerCapture(pointer_id);
    }
  }

  function finish(worker: Drag) {
    if (drag.current !== worker) return;
    drag.current = null;
    release(worker);
    setPreview(null);
    if (in_flight.current !== worker) latest.current.on_busy(false);
  }

  function cancel() {
    const worker = drag.current;
    if (worker) finish(worker);
  }
  const cancel_ref = useRef(cancel);
  cancel_ref.current = cancel;

  useEffect(() => {
    if (drag.current && (drag.current.signature !== identity || !owned)) cancel_ref.current();
  }, [identity, owned]);
  useEffect(() => {
    const blur = () => cancel_ref.current();
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || !drag.current) return;
      event.preventDefault();
      event.stopPropagation();
      cancel_ref.current();
    };
    window.addEventListener("blur", blur);
    window.addEventListener("keydown", escape, true);
    return () => {
      window.removeEventListener("blur", blur);
      window.removeEventListener("keydown", escape, true);
      cancel_ref.current();
    };
  }, []);

  async function drain(worker: Drag) {
    if (worker.running) return;
    worker.running = true;
    in_flight.current = worker;
    try {
      while (drag.current === worker && worker.pending !== null) {
        const position = worker.pending;
        worker.pending = null;
        const next = await resizeSessionDivider(worker.session, {
          view_id: worker.baseline.view_id, expected_revision: worker.baseline.revision,
          split_path: worker.divider.split_path, boundary: worker.divider.boundary, position,
        });
        if (drag.current !== worker) return;
        const current = currentView(latest.current);
        if (signature(latest.current) !== worker.signature || !sessionLayoutOwned(worker.session) ||
          next.view_id !== worker.baseline.view_id || current?.view_id === next.view_id && BigInt(current.revision) > BigInt(next.revision)) {
          finish(worker);
          return;
        }
        worker.baseline = next;
        latest.current.on_confirm(next);
      }
    } catch (failure) {
      if (drag.current === worker) {
        latest.current.on_error(errorMessage(failure));
        finish(worker);
      }
    } finally {
      worker.running = false;
      const owns_flight = in_flight.current === worker;
      if (owns_flight) in_flight.current = null;
      if (drag.current === worker && worker.pointer_id === null && worker.pending === null) finish(worker);
      else if (owns_flight && !drag.current) latest.current.on_busy(false);
    }
  }

  function begin(divider: ViewDivider, element: HTMLElement): Drag | null {
    const { session, enabled, can_begin } = latest.current;
    const view = currentView(latest.current);
    if (!session || !view || !enabled || drag.current || in_flight.current || !can_begin()) return null;
    // The handle's axis, path and anchor come from the rendered snapshot. A
    // newer synchronous view can guard ACKs, but cannot reinterpret this handle.
    const rendered = latest.current.view;
    if (rendered?.view_id !== view.view_id || rendered.revision !== view.revision) return null;
    if (!sessionLayoutOwned(session)) { latest.current.on_error("Take resize control to resize panes."); return null; }
    const position = Math.floor(divider.vertical ? divider.left : divider.top);
    const worker: Drag = {
      divider, baseline: view, session, signature: signature(latest.current), element,
      pointer_id: null, coordinate: 0, initial_position: position, latest_position: position, pending: null, running: false,
    };
    drag.current = worker;
    latest.current.on_error(null);
    latest.current.on_busy(true);
    return worker;
  }

  function schedule(worker: Drag, position: number) {
    if (drag.current !== worker || !sessionLayoutOwned(worker.session)) { finish(worker); return; }
    const extent = worker.divider.vertical ? worker.baseline.canvas_size.columns : worker.baseline.canvas_size.rows;
    const target = Math.max(0, Math.min(extent - 1, position));
    if (target === worker.latest_position) return;
    worker.latest_position = target;
    worker.pending = target;
    setPreview({ path: worker.divider.path, position: target });
    void drain(worker);
  }

  function move(event: PointerEvent<HTMLElement>, worker: Drag) {
    const cell = latest.current.cell;
    const coordinate = worker.divider.vertical ? event.clientX : event.clientY;
    const size = worker.divider.vertical ? cell.width : cell.height;
    schedule(worker, worker.initial_position + Math.round((coordinate - worker.coordinate) / size));
  }

  function handlers(divider: ViewDivider) {
    return {
      onPointerDown: (event: PointerEvent<HTMLElement>) => {
        if (event.button !== 0 || event.isPrimary === false) return;
        const worker = begin(divider, event.currentTarget);
        if (!worker) return;
        event.preventDefault();
        event.stopPropagation();
        worker.pointer_id = event.pointerId;
        worker.coordinate = divider.vertical ? event.clientX : event.clientY;
        try { event.currentTarget.setPointerCapture(event.pointerId); }
        catch { finish(worker); }
      },
      onPointerMove: (event: PointerEvent<HTMLElement>) => {
        const worker = drag.current;
        if (!worker || worker.pointer_id !== event.pointerId) return;
        event.preventDefault();
        move(event, worker);
      },
      onPointerUp: (event: PointerEvent<HTMLElement>) => {
        const worker = drag.current;
        if (!worker || worker.pointer_id !== event.pointerId) return;
        event.preventDefault();
        move(event, worker);
        release(worker);
        if (!worker.running && worker.pending === null) finish(worker);
      },
      onPointerCancel: (event: PointerEvent<HTMLElement>) => {
        if (drag.current?.pointer_id === event.pointerId) cancel();
      },
      onLostPointerCapture: (event: PointerEvent<HTMLElement>) => {
        if (drag.current?.pointer_id === event.pointerId) cancel();
      },
      onKeyDown: (event: KeyboardEvent<HTMLElement>) => {
        if (event.shiftKey || event.metaKey || event.ctrlKey && event.altKey) return;
        const direction = divider.vertical ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
        const delta = event.key === direction[0] ? -1 : event.key === direction[1] ? 1 : 0;
        if (!delta) return;
        const worker = drag.current ?? begin(divider, event.currentTarget);
        if (!worker || worker.pointer_id !== null || worker.divider.path !== divider.path) return;
        event.preventDefault();
        schedule(worker, worker.latest_position + delta * (event.altKey ? 5 : 1));
        if (!worker.running && worker.pending === null) finish(worker);
      },
    };
  }
  return { handlers, preview, owned };
}
