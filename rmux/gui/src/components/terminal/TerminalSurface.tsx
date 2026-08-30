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
  hasSession: boolean;
  onInput(data: Uint8Array): void;
  onReady(renderer: XtermRenderer | null): void;
}

export function TerminalSurface({
  phase,
  hasSession,
  onInput,
  onReady,
}: TerminalSurfaceProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef(onInput);
  const readyRef = useRef(onReady);
  inputRef.current = onInput;
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
    readyRef.current(renderer);
    return () => {
      readyRef.current(null);
      renderer.dispose();
    };
  }, []);

  return (
    <div className="terminal-shell">
      <div className="terminal-scroll-region">
        <div ref={containerRef} className="terminal-container" />
      </div>
      {!hasSession && phase === "idle" ? (
        <div className="terminal-placeholder">
          <span className="terminal-mark">›_</span>
          <h2>A terminal that outlives its window.</h2>
          <p>Select a running session or create a new shell.</p>
        </div>
      ) : null}
      {phase === "connecting" || phase === "reconnecting" ? (
        <div className="terminal-overlay">
          <span className="spinner" aria-hidden="true" />
          {phase === "reconnecting" ? "Reconnecting…" : "Attaching…"}
        </div>
      ) : null}
    </div>
  );
}
