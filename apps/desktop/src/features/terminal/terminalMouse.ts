import type { IDisposable, Terminal } from "@xterm/xterm";
import { TerminalSelection } from "./terminalSelection";

interface MouseActions {
  link_at(event: MouseEvent): string | null;
  on_open_link(url: string): void;
  on_copy_text(text: string): void;
}

interface MouseDocument extends IDisposable {
  terminals: Set<Terminal>;
  forwarding: boolean;
  gesture: MouseGesture | null;
  meta_down: boolean;
  ctrl_down: boolean;
}

interface MouseGesture {
  cancelled: boolean;
  cancel(): void;
  finish(): void;
}

const mouse_documents = new WeakMap<Document, MouseDocument>();

function mouseDocument(document: Document): MouseDocument {
  const existing = mouse_documents.get(document);
  if (existing) return existing;
  const window = document.defaultView!;
  const keyboard = (event: KeyboardEvent) => {
    // Observe before xterm or command handlers consume the event. A held key
    // remains active until release, even if a mouse event omits its modifier.
    const down = event.type === "keydown";
    shared.meta_down = event.metaKey || down && event.key === "Meta";
    shared.ctrl_down = event.ctrlKey || down && event.key === "Control";
  };
  const reset = () => {
    shared.meta_down = false;
    shared.ctrl_down = false;
    shared.gesture?.cancel();
  };
  const visibility = () => { if (document.hidden) reset(); };
  const newPress = (event: MouseEvent) => {
    // A new physical press ends a lost gesture, including presses in controls
    // outside the terminal. It must remain available to its actual target.
    if (!shared.forwarding && event.button === 0) shared.gesture?.finish();
  };
  const cancelledInput = (event: MouseEvent) => {
    if (shared.forwarding || !shared.gesture?.cancelled || event.type === "mouseup" && event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.type === "mouseup") shared.gesture.finish();
  };
  const shared: MouseDocument = {
    terminals: new Set(), forwarding: false, gesture: null,
    meta_down: false, ctrl_down: false,
    dispose() {
      window.removeEventListener("keydown", keyboard, true);
      window.removeEventListener("keyup", keyboard, true);
      window.removeEventListener("blur", reset);
      document.removeEventListener("visibilitychange", visibility);
      document.removeEventListener("mousedown", newPress, true);
      document.removeEventListener("mousemove", cancelledInput, true);
      document.removeEventListener("mouseup", cancelledInput, true);
      document.removeEventListener("wheel", cancelledInput, true);
      shared.gesture?.finish();
      mouse_documents.delete(document);
    },
  };
  window.addEventListener("keydown", keyboard, true);
  window.addEventListener("keyup", keyboard, true);
  window.addEventListener("blur", reset);
  document.addEventListener("visibilitychange", visibility);
  document.addEventListener("mousedown", newPress, true);
  document.addEventListener("mousemove", cancelledInput, true);
  document.addEventListener("mouseup", cancelledInput, true);
  document.addEventListener("wheel", cancelledInput, { capture: true, passive: false });
  mouse_documents.set(document, shared);
  return shared;
}

/** Select and copy locally without entering xterm's mouse-selection state. */
export function installTerminalMouse(terminal: Terminal, is_mac: boolean, actions: MouseActions): IDisposable {
  const element = terminal.element!;
  const screen = element.querySelector<HTMLElement>(".xterm-screen")!;
  const document = element.ownerDocument;
  const window = document.defaultView!;
  const selection = new TerminalSelection(terminal, screen);
  const shared = mouseDocument(document);
  const modifierHeld = (event: MouseEvent) => is_mac ? event.metaKey || shared.meta_down : event.ctrlKey || shared.ctrl_down;
  const modified = (event: MouseEvent) => event.button === 0 && modifierHeld(event);
  shared.terminals.add(terminal);
  let pressed: { event: MouseEvent; link: string | null; dragging: boolean } | null = null;
  let last_move: MouseEvent | null = null;
  let scroll_timer: number | null = null;

  const locally = (action: () => void) => {
    // xterm installs drag/release listeners on the document, including for
    // other panes. Silence every renderer while dispatching local events,
    // before their input callbacks can clear selection or scroll to the prompt.
    const previous = [...shared.terminals].map((item) => [item, item.options.disableStdin] as const);
    const was_forwarding = shared.forwarding;
    shared.forwarding = true;
    try {
      for (const [item] of previous) item.options.disableStdin = true;
      action();
    } finally {
      for (const [item, disabled] of previous) {
        if (shared.terminals.has(item)) item.options.disableStdin = disabled;
      }
      shared.forwarding = was_forwarding;
    }
  };

  const consume = (event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
  };
  const visible = () => {
    const rect = screen.getBoundingClientRect();
    return screen.isConnected && rect.width > 0 && rect.height > 0;
  };
  const inside = (event: MouseEvent) => {
    // xterm handles mouse presses over its whole root, including the blank
    // space around the cell grid. Claim that same area; selection clamps the
    // anchor to the grid instead of letting gutter presses reach the program.
    return event.target instanceof Node && element.contains(event.target) && visible();
  };
  const hover = (event: MouseEvent) => {
    // Keep URL discovery working while the modifier is held. This is only a
    // hover: selection never dispatches synthetic presses, drags or releases.
    locally(() => {
      screen.dispatchEvent(new window.MouseEvent("mousemove", {
        bubbles: true, cancelable: true, buttons: 0,
        clientX: event.clientX, clientY: event.clientY,
      }));
    });
  };
  const clearPreview = () => {
    last_move = null;
    if (scroll_timer !== null) window.clearInterval(scroll_timer);
    scroll_timer = null;
    selection.clear();
  };
  const cancel = () => {
    if (pressed) gesture.cancelled = true;
    clearPreview();
  };
  const finish = () => {
    if (!gesture.cancelled) clearPreview();
    pressed = null;
    if (shared.gesture === gesture) shared.gesture = null;
    gesture.cancelled = false;
  };
  const gesture: MouseGesture = { cancelled: false, cancel, finish };
  const refreshSelection = () => {
    if (gesture.cancelled || !pressed?.dragging || !last_move) return;
    if (!visible()) { cancel(); return; }
    if (!selection.update(last_move)) cancel();
  };
  const autoScroll = () => {
    if (gesture.cancelled || !pressed?.dragging || !last_move) return;
    if (!visible()) { cancel(); return; }
    const rect = screen.getBoundingClientRect();
    const distance = last_move.clientY < rect.top ? last_move.clientY - rect.top :
      last_move.clientY >= rect.bottom ? last_move.clientY - rect.bottom + 1 : 0;
    if (!distance) return;
    const rows = Math.sign(distance) * Math.min(10, Math.max(1, Math.ceil(Math.abs(distance) / (rect.height / terminal.rows))));
    terminal.scrollLines(rows);
    refreshSelection();
  };
  const update = (event: MouseEvent) => {
    if (!pressed || gesture.cancelled) return;
    last_move = event;
    if (!pressed.dragging && Math.hypot(event.clientX - pressed.event.clientX, event.clientY - pressed.event.clientY) >= 4) {
      pressed.dragging = true;
      scroll_timer = window.setInterval(autoScroll, 50);
    }
    refreshSelection();
  };
  const down = (event: MouseEvent) => {
    if (shared.forwarding || !inside(event)) return;
    if (!modified(event)) return;
    pressed = { event, link: actions.link_at(event), dragging: false };
    gesture.cancelled = false;
    shared.gesture = gesture;
    consume(event);
    selection.begin(event);
    locally(() => {
      // A lost ordinary selection release in another pane can leave a native
      // document listener behind. Our local bounds replace those selections.
      for (const item of shared.terminals) item.clearSelection();
      terminal.focus();
    });
  };
  const move = (event: MouseEvent) => {
    if (shared.forwarding || (shared.gesture && shared.gesture !== gesture)) return;
    if (!pressed) {
      if (modifierHeld(event) && inside(event)) {
        consume(event);
        // Preserve local link hover, but don't let ANY-mode apps observe the
        // pointer before a drag or erase the selection immediately afterward.
        if (!event.buttons) hover(event);
      }
      return;
    }
    consume(event);
    if (gesture.cancelled) return;
    if (!(event.buttons & 1) || !visible()) {
      cancel();
      return;
    }
    update(event);
  };
  const up = (event: MouseEvent) => {
    if (shared.forwarding || !pressed || event.button !== 0) return;
    consume(event);
    if (!visible()) {
      cancel();
      finish();
      return;
    }
    // The browser may coalesce movement before release; include its final cell.
    update(event);
    const completed = gesture.cancelled ? null : pressed;
    const text = completed?.dragging ? selection.text() : "";
    finish(); // Remove the bounds before invoking the clipboard operation.
    if (completed?.dragging) {
      if (text) actions.on_copy_text(text);
    } else if (completed?.link && modified(event) && inside(event) && actions.link_at(event) === completed.link) {
      actions.on_open_link(completed.link);
    }
  };
  const wheel = (event: WheelEvent) => {
    if (shared.forwarding || (shared.gesture && shared.gesture !== gesture) ||
      (!pressed && (!modifierHeld(event) || !inside(event)))) return;
    consume(event);
    if (gesture.cancelled) return;
    // Scroll only local history; never send wheel reports or alternate-screen
    // arrow keys to the program while selecting.
    if (!visible()) { cancel(); return; }
    const height = screen.getBoundingClientRect().height / terminal.rows;
    const delta = event.deltaMode === 1 ? event.deltaY : event.deltaMode === 2 ? event.deltaY * terminal.rows : event.deltaY / height;
    if (delta) terminal.scrollLines(Math.sign(delta) * Math.max(1, Math.round(Math.abs(delta))));
    refreshSelection();
  };

  element.addEventListener("mousedown", down, true);
  document.addEventListener("mousemove", move, true);
  document.addEventListener("mouseup", up, true);
  document.addEventListener("wheel", wheel, { capture: true, passive: false });
  const resize = terminal.onResize(cancel);
  const buffer_change = terminal.buffer.onBufferChange(cancel);
  const scroll = terminal.onScroll(refreshSelection);
  const write = terminal.onWriteParsed(refreshSelection);
  return {
    dispose() {
      if (!shared.terminals.delete(terminal)) return;
      cancel();
      element.removeEventListener("mousedown", down, true);
      document.removeEventListener("mousemove", move, true);
      document.removeEventListener("mouseup", up, true);
      document.removeEventListener("wheel", wheel, true);
      resize.dispose();
      buffer_change.dispose();
      scroll.dispose();
      write.dispose();
      selection.dispose();
      if (!shared.terminals.size) shared.dispose();
    },
  };
}
