import { Icon } from "../ui/Icon";
import { useEffect, useId, useRef } from "react";
import type { ConnectionPhase, TerminalSize } from "../../lib/types";
import { errorMessage } from "../../lib/errors";
import { openExternalUrl } from "../../lib/tauri";
import { XtermRenderer } from "../../features/terminal/XtermRenderer";
import { useNotificationEnvironment } from "../../features/notifications/NotificationContext";
import "./TerminalSurface.css";

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
  on_reconnect?(): Promise<void> | void;
  on_reconnect_interaction?(): Promise<void> | void;
  reconnect_available?: boolean;
  reconnect_error?: string | null;
  retry_paused?: boolean;
  interaction_enabled?: boolean;
  hasSession: boolean;
  has_cached_content: boolean;
  onInput(data: Uint8Array): void;
  onReady(renderer: XtermRenderer | null): void;
}

export function TerminalSurface({
  phase,
  ended_message,
  on_dismiss,
  on_reconnect,
  on_reconnect_interaction,
  reconnect_available = true,
  reconnect_error,
  retry_paused = false,
  interaction_enabled = true,
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
  inputRef.current = ended_message || phase !== "attached" ? () => {} : onInput;
  readyRef.current = onReady;
  notificationsRef.current = notifications;
  const disconnected = hasSession && !ended_message && (["disconnected", "error", "retry_wait", "reconnecting"].includes(phase) || phase === "connecting" && has_cached_content);
  const can_reconnect = reconnect_available && interaction_enabled && ["disconnected", "error", "retry_wait"].includes(phase);
  const reconnect_label = phase === "connecting" ? "Connecting…" : phase === "reconnecting" || phase === "retry_wait" ? "Reconnecting…" : retry_paused ? "Retries paused" : "Disconnected";
  const requestReconnect = (callback?: () => Promise<void> | void) => {
    if (!callback) return;
    // Attachment state and notifications own reconnect failures.
    try { void Promise.resolve(callback()).catch(() => {}); } catch { /* Reported by the attachment owner. */ }
  };

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
    <div className={`terminal-shell${disconnected ? " terminal-disconnected" : ""}`} onKeyDownCapture={(event) => {
      if (ended_message) {
        if (!on_dismiss || ["Shift", "Control", "Alt", "Meta"].includes(event.key)) return;
        event.preventDefault();
        event.stopPropagation();
        on_dismiss();
        return;
      }
      if (!disconnected || !interaction_enabled || event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey ||
        event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229 || !(event.target instanceof Node) ||
        !containerRef.current?.contains(event.target)) return;
      const typing = event.key.length === 1;
      const navigation = !event.shiftKey && ["Enter", "Backspace", "Delete", "Tab", "Escape", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown"].includes(event.key);
      if (!typing && !navigation) return;
      event.preventDefault();
      event.stopPropagation();
      if (can_reconnect && !event.repeat) requestReconnect(on_reconnect_interaction);
    }}>
      {ended_message && <div className="pane-message" role="status">
        {ended_message} — press any key to close.
        <button type="button" onClick={on_dismiss}>Close</button>
      </div>}
      <div className="terminal-scroll-region">
        <div ref={containerRef} className="terminal-container" />
      </div>
      {disconnected && <div className="terminal-connection" role="status" aria-live="polite" aria-atomic="true">
        <span className={`terminal-connection-dot${phase === "connecting" || phase === "reconnecting" || phase === "retry_wait" ? " terminal-connection-pending" : ""}`} aria-hidden="true" />
        <span title={reconnect_error ?? undefined}>{reconnect_label}</span>
        {on_reconnect && reconnect_available && <button type="button" disabled={!can_reconnect}
          title={reconnect_error ?? "Reconnect to this terminal"} onClick={() => requestReconnect(on_reconnect)}>Reconnect</button>}
      </div>}
      {!hasSession && phase === "idle" ? (
        <div className="terminal-placeholder">
          <span className="terminal-mark"><Icon name="terminal" size={64} /></span>
          <h2>A terminal that outlives its window.</h2>
          <p>Select a remembered session to connect, or create a new shell.</p>
        </div>
      ) : null}
      {!has_cached_content && phase === "connecting" ? (
        <div className="terminal-overlay">
          <span className="spinner" aria-hidden="true" />
          Attaching…
        </div>
      ) : null}
    </div>
  );
}
