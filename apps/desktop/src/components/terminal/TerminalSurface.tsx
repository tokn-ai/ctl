import { Icon } from "../ui/Icon";
import { useEffect, useId, useRef } from "react";
import type { ConnectionPhase, TerminalSize } from "../../lib/types";
import { errorMessage } from "../../lib/errors";
import { openExternalUrl } from "../../lib/tauri";
import { XtermRenderer } from "../../features/terminal/XtermRenderer";
import { useNotificationEnvironment } from "../../features/notifications/NotificationContext";

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
  const inputRef = useRef(onInput);
  const readyRef = useRef(onReady);
  const surface_id = useId();
  const link_notification_key = `terminal-link:${surface_id}`;
  const copy_notification_key = `terminal-copy:${surface_id}`;
  const notifications = useNotificationEnvironment()?.store;
  const notificationsRef = useRef(notifications);
  inputRef.current = ended_message ? () => {} : onInput;
  readyRef.current = onReady;
  notificationsRef.current = notifications;

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    let disposed = false;
    let open_generation = 0;
    let copy_generation = 0;
    const copyText = async (text: string) => {
      if (disposed || !text) return;
      const generation = ++copy_generation;
      const store = notificationsRef.current;
      store?.report(copy_notification_key, null);
      try {
        if (!navigator.clipboard?.writeText) throw new Error("Clipboard access is unavailable.");
        await navigator.clipboard.writeText(text);
        if (!disposed && generation === copy_generation) store?.resolve(copy_notification_key);
      } catch (failure) {
        if (!disposed && generation === copy_generation) store?.report(copy_notification_key, {
          severity: "error",
          title: "Could not copy selection",
          message: errorMessage(failure),
          source: "Terminal",
        });
      }
    };
    const renderer = new XtermRenderer(
      container,
      (data) => inputRef.current(data),
      INITIAL_SIZE,
      (url) => {
        if (disposed) return;
        const generation = ++open_generation;
        const store = notificationsRef.current;
        store?.report(link_notification_key, null);
        void openExternalUrl(url).then(() => {
          if (!disposed && generation === open_generation) store?.resolve(link_notification_key);
        }, (failure) => {
          if (!disposed && generation === open_generation) store?.report(link_notification_key, {
            severity: "error",
            title: "Could not open link",
            message: errorMessage(failure),
            source: "Terminal",
          });
        });
      },
      (text) => { void copyText(text); },
    );
    readyRef.current(renderer);
    return () => {
      disposed = true;
      readyRef.current(null);
      renderer.dispose();
    };
  }, [link_notification_key, copy_notification_key]);

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
      {!has_cached_content && (phase === "connecting" || phase === "reconnecting" || phase === "retry_wait") ? (
        <div className="terminal-overlay">
          <span className="spinner" aria-hidden="true" />
          {phase === "retry_wait" ? "Waiting to retry…" : phase === "reconnecting" ? "Reconnecting…" : "Attaching…"}
        </div>
      ) : null}
    </div>
  );
}
