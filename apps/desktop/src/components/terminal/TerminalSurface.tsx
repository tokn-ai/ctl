import { Icon } from "../ui/Icon";
import { useEffect, useRef } from "react";
import type { ConnectionPhase, TerminalSize } from "../../lib/types";
import { XtermRenderer } from "../../features/terminal/XtermRenderer";

const INITIAL_SIZE: TerminalSize = {
  columns: 80,
  rows: 24,
  pixel_width: null,
  pixel_height: null,
};

interface TerminalSurfaceProps {
  phase: ConnectionPhase;
  ended_message?: string | null;
  on_dismiss?(): void;
  hasSession: boolean;
  has_cached_content: boolean;
  onInput(data: Uint8Array): void;
  onReady(renderer: XtermRenderer | null): void;
}

export function TerminalSurface({
  phase,
  ended_message,
  on_dismiss,
  hasSession,
  has_cached_content,
  onInput,
  onReady,
}: TerminalSurfaceProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const rendererRef = useRef<XtermRenderer | null>(null);
  const read_only = !ended_message && phase !== "attached" && phase !== "ended";
  const inputRef = useRef(onInput);
  const readyRef = useRef(onReady);
  inputRef.current = ended_message || phase !== "attached" ? () => {} : onInput;
  readyRef.current = onReady;

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    const renderer = new XtermRenderer(
      container,
      (data) => inputRef.current(data),
      INITIAL_SIZE,
    );
    rendererRef.current = renderer;
    readyRef.current(renderer);
    return () => {
      rendererRef.current = null;
      readyRef.current(null);
      renderer.dispose();
    };
  }, []);

  useEffect(() => { rendererRef.current?.setReadOnly(read_only); }, [read_only]);

  return (
    <div className="terminal-shell" onKeyDownCapture={(event) => {
      if (!ended_message || !on_dismiss || ["Shift", "Control", "Alt", "Meta"].includes(event.key)) return;
      event.preventDefault();
      event.stopPropagation();
      on_dismiss();
    }}>
      {ended_message && <div className="pane-message" role="status">
        {ended_message} — press any key to close.
        <button type="button" onClick={on_dismiss}>Close</button>
      </div>}
      <div className="terminal-scroll-region">
        <div ref={containerRef} className="terminal-container" />
      </div>
      {!hasSession && phase === "idle" ? (
        <div className="terminal-placeholder">
          <span className="terminal-mark"><Icon name="terminal" size={64} /></span>
          <h2>A terminal that outlives its window.</h2>
          <p>Select a remembered session to connect, or create a new shell.</p>
        </div>
      ) : null}
      {hasSession && !has_cached_content && phase === "disconnected" && !ended_message ? (
        <div className="terminal-placeholder"><p>No cached output on this device. Connect the host to view this session.</p></div>
      ) : null}
      {!has_cached_content && (phase === "connecting" || phase === "reconnecting") ? (
        <div className="terminal-overlay">
          <span className="spinner" aria-hidden="true" />
          {phase === "reconnecting" ? "Reconnecting…" : "Attaching…"}
        </div>
      ) : null}
    </div>
  );
}
