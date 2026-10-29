import type { IBuffer, IDisposable, IMarker, Terminal } from "@xterm/xterm";

interface SelectionPoint {
  column: number;
  row: number;
}

interface SelectionRange {
  start: SelectionPoint;
  end: SelectionPoint;
}

function comparePoints(left: SelectionPoint, right: SelectionPoint): number {
  return left.row - right.row || left.column - right.column;
}

/** A local selection preview. It never changes xterm's native selection state. */
export class TerminalSelection {
  private anchor: SelectionPoint | null = null;
  private anchor_buffer: IBuffer | null = null;
  private anchor_marker: IMarker | null = null;
  private marker_listener: IDisposable | null = null;
  private last_event: MouseEvent | null = null;
  private overlay: HTMLDivElement | null = null;

  constructor(private readonly terminal: Terminal, private readonly screen: HTMLElement) {}

  begin(event: MouseEvent): void {
    this.clear();
    const anchor = this.pointAt(event);
    if (!anchor) return;
    this.anchor = anchor;
    this.anchor_buffer = this.terminal.buffer.active;
    if (this.anchor_buffer.type === "normal") {
      const marker = this.terminal.registerMarker(anchor.row - this.anchor_buffer.baseY - this.anchor_buffer.cursorY);
      if (!marker) { this.clear(); return; }
      this.anchor_marker = marker;
      this.marker_listener = marker.onDispose(() => this.clear());
    }
  }

  update(event: MouseEvent): boolean {
    if (!this.anchor) return false;
    this.last_event = event;
    const range = this.range();
    this.overlay?.remove();
    this.overlay = null;
    if (!range) return false;
    if (comparePoints(range.start, range.end) === 0) return true;

    const { cols, rows } = this.terminal;
    const viewport = this.terminal.buffer.active.viewportY;
    const rectangles: HTMLDivElement[] = [];
    for (let row = Math.max(range.start.row, viewport); row <= Math.min(range.end.row, viewport + rows - 1); row += 1) {
      const start = row === range.start.row ? range.start.column : 0;
      const end = row === range.end.row ? range.end.column : cols;
      if (start >= end) continue;
      const rectangle = this.screen.ownerDocument.createElement("div");
      rectangle.className = "terminal-selection-rect";
      Object.assign(rectangle.style, {
        position: "absolute", left: `${start / cols * 100}%`, width: `${(end - start) / cols * 100}%`,
        top: `${(row - viewport) / rows * 100}%`, height: `${100 / rows}%`,
        backgroundColor: this.terminal.options.theme?.selectionBackground ?? "#264f78",
        opacity: "0.55", border: "1px solid rgba(170, 205, 255, 0.65)", boxSizing: "border-box",
      });
      rectangles.push(rectangle);
    }
    if (!rectangles.length) return true;
    const overlay = this.screen.ownerDocument.createElement("div");
    overlay.className = "terminal-selection-overlay";
    overlay.setAttribute("aria-hidden", "true");
    Object.assign(overlay.style, { position: "absolute", inset: "0", zIndex: "7", pointerEvents: "none", userSelect: "none" });
    overlay.append(...rectangles);
    this.screen.append(overlay);
    this.overlay = overlay;
    return true;
  }

  text(): string {
    const range = this.range();
    if (!range || comparePoints(range.start, range.end) === 0) return "";
    const buffer = this.terminal.buffer.active;
    const lines: string[] = [];
    for (let row = range.start.row; row <= range.end.row; row += 1) {
      const line = buffer.getLine(row);
      if (!line) continue;
      const start = row === range.start.row ? range.start.column : 0;
      const end = row === range.end.row ? range.end.column : this.terminal.cols;
      // xterm's cell-aware translation preserves combined/wide glyphs and
      // printed spaces, while trimRight removes only unprinted terminal padding.
      const text = line.translateToString(true, start, end);
      if (row !== range.start.row && line.isWrapped) lines[lines.length - 1] += text;
      else lines.push(text);
    }
    return lines.join("\n");
  }

  clear(): void {
    this.marker_listener?.dispose();
    this.marker_listener = null;
    const marker = this.anchor_marker;
    this.anchor_marker = null;
    this.anchor = null;
    this.anchor_buffer = null;
    this.last_event = null;
    this.overlay?.remove();
    this.overlay = null;
    marker?.dispose();
  }

  dispose(): void { this.clear(); }

  private range(): SelectionRange | null {
    if (!this.anchor || !this.last_event) return null;
    if (this.anchor_buffer !== this.terminal.buffer.active) {
      this.clear();
      return null;
    }
    const focus = this.pointAt(this.last_event);
    const row = this.anchor_marker?.line ?? this.anchor.row;
    if (!focus || row < 0 || row >= this.terminal.buffer.active.length) {
      this.clear();
      return null;
    }
    const anchor = { row, column: this.wholeCellBoundary(row, this.anchor.column) };
    return comparePoints(anchor, focus) <= 0 ? { start: anchor, end: focus } : { start: focus, end: anchor };
  }

  private pointAt(event: MouseEvent): SelectionPoint | null {
    const rect = this.screen.getBoundingClientRect();
    const { cols, rows, buffer } = this.terminal;
    if (rect.width <= 0 || rect.height <= 0 || buffer.active.length === 0) return null;
    const viewport_row = Math.max(0, Math.min(rows - 1, Math.floor((event.clientY - rect.top) * rows / rect.height)));
    const row = Math.min(buffer.active.length - 1, buffer.active.viewportY + viewport_row);
    // Select cell boundaries, including the right edge after the final cell.
    // Above/below the viewport, edge scrolling selects whole physical rows.
    const column = event.clientY < rect.top ? 0 : event.clientY >= rect.bottom ? cols :
      Math.max(0, Math.min(cols, Math.ceil((event.clientX - rect.left) * cols / rect.width + 0.5) - 1));
    return { row, column: this.wholeCellBoundary(row, column) };
  }

  private wholeCellBoundary(row: number, column: number): number {
    const clamped = Math.max(0, Math.min(this.terminal.cols, column));
    return clamped < this.terminal.cols && this.terminal.buffer.active.getLine(row)?.getCell(clamped)?.getWidth() === 0 ? clamped + 1 : clamped;
  }
}
