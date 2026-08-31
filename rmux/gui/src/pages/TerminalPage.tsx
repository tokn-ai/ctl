import { useCallback, useEffect, useRef, useState } from "react";
import { SessionSidebar } from "../components/sessions/SessionSidebar";
import { StatusBar } from "../components/status/StatusBar";
import { TerminalSurface } from "../components/terminal/TerminalSurface";
import { TerminalToolbar } from "../components/terminal/TerminalToolbar";
import { useAttachment } from "../features/attachment/useAttachment";
import {
  SessionListRefreshGuard,
  prependSession,
  removeSession,
  replaceSessionList,
  syncSessionTerminalSize,
} from "../features/sessions/sessionListState";
import type { XtermRenderer } from "../features/terminal/XtermRenderer";
import { errorCode, errorMessage } from "../lib/errors";
import {
  createSession,
  killSession,
  listSessions,
} from "../lib/tauri";
import type { SessionSummary, TerminalSize } from "../lib/types";

function measuredSize(renderer: XtermRenderer | null): TerminalSize {
  const proposed = renderer?.proposeDimensions();
  return {
    columns: proposed?.columns ?? 80,
    rows: proposed?.rows ?? 24,
    pixel_width: null,
    pixel_height: null,
  };
}

export function TerminalPage() {
  const [renderer, setRenderer] = useState<XtermRenderer | null>(null);
  const attachment = useAttachment(renderer);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [closingSessionIds, setClosingSessionIds] = useState<ReadonlySet<string>>(
    new Set(),
  );
  const [disconnectingSessionId, setDisconnectingSessionId] = useState<
    string | null
  >(null);
  const closingSessionIdsRef = useRef(new Set<string>());
  const closedSessionIdsRef = useRef(new Set<string>());
  const refreshGuardRef = useRef(new SessionListRefreshGuard());

  const refresh = useCallback(async () => {
    const token = refreshGuardRef.current.begin();
    setLoading(true);
    setListError(null);
    try {
      const listed = await listSessions();
      if (!refreshGuardRef.current.canApply(token)) {
        return;
      }
      const listedIds = new Set(listed.map((session) => session.session_id));
      const hidden = closedSessionIdsRef.current;
      setSessions(
        replaceSessionList(
          listed.filter((session) => !hidden.has(session.session_id)),
        ),
      );
      for (const sessionId of hidden) {
        if (!listedIds.has(sessionId)) {
          hidden.delete(sessionId);
        }
      }
    } catch (error) {
      if (refreshGuardRef.current.canApply(token)) {
        setListError(errorMessage(error));
      }
    } finally {
      if (refreshGuardRef.current.isLatest(token)) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const create = useCallback(
    async (workingDirectory: string | null): Promise<boolean> => {
      setCreating(true);
      setListError(null);
      try {
        const session = await createSession({
          working_directory: workingDirectory,
          terminal_size: measuredSize(renderer),
        });
        refreshGuardRef.current.recordMutation();
        setSessions((current) => prependSession(current, session));
        await attachment.connect(session, { resize_with_window: true });
        return true;
      } catch (error) {
        setListError(errorMessage(error));
        return false;
      } finally {
        setCreating(false);
      }
    },
    [attachment, renderer],
  );

  const disconnect = useCallback(
    async (session: SessionSummary) => {
      if (attachment.state.session?.session_id !== session.session_id) {
        return;
      }
      setDisconnectingSessionId(session.session_id);
      setListError(null);
      try {
        await attachment.detach();
      } finally {
        setDisconnectingSessionId((current) =>
          current === session.session_id ? null : current,
        );
      }
    },
    [attachment],
  );

  const close = useCallback(async (session: SessionSummary) => {
    if (closingSessionIdsRef.current.has(session.session_id)) {
      return;
    }
    closingSessionIdsRef.current.add(session.session_id);
    setClosingSessionIds((current) => {
      const next = new Set(current);
      next.add(session.session_id);
      return next;
    });
    setListError(null);
    try {
      try {
        await killSession({ session_id: session.session_id });
      } catch (error) {
        if (errorCode(error) !== "session_not_found") {
          setListError(errorMessage(error));
          return;
        }
      }
      refreshGuardRef.current.recordMutation();
      closedSessionIdsRef.current.add(session.session_id);
      setSessions((current) => removeSession(current, session.session_id));
      // The session is hidden after either an accepted kill or a not-found
      // response, which means another actor already achieved the same result.
    } finally {
      closingSessionIdsRef.current.delete(session.session_id);
      setClosingSessionIds((current) => {
        const next = new Set(current);
        next.delete(session.session_id);
        return next;
      });
    }
  }, []);

  const attachedSession = attachment.state.session;
  const attachedTerminalSize = attachedSession?.terminal_size;
  useEffect(() => {
    if (!attachedSession || !attachedTerminalSize) {
      return;
    }
    refreshGuardRef.current.recordMutation();
    setSessions((current) =>
      syncSessionTerminalSize(
        current,
        attachedSession.session_id,
        attachedTerminalSize,
      ),
    );
  }, [
    attachedSession?.session_id,
    attachedTerminalSize?.columns,
    attachedTerminalSize?.rows,
    attachedTerminalSize?.pixel_width,
    attachedTerminalSize?.pixel_height,
  ]);

  useEffect(() => {
    if (attachment.state.phase !== "ended" || !attachedSession) {
      return;
    }
    refreshGuardRef.current.recordMutation();
    closedSessionIdsRef.current.add(attachedSession.session_id);
    setSessions((current) => removeSession(current, attachedSession.session_id));
  }, [attachment.state.phase, attachedSession]);

  const disconnectableSessionId =
    attachedSession && attachment.state.phase !== "ended"
      ? attachedSession.session_id
      : null;

  return (
    <main className="app-shell">
      <SessionSidebar
        sessions={sessions}
        selectedSessionId={attachedSession?.session_id ?? null}
        disconnectableSessionId={disconnectableSessionId}
        loading={loading}
        error={listError}
        creating={creating}
        closingSessionIds={closingSessionIds}
        disconnectingSessionId={disconnectingSessionId}
        onRefresh={() => void refresh()}
        onSelect={(session) => void attachment.connect(session)}
        onCreate={create}
        onDisconnect={(session) => void disconnect(session)}
        onClose={(session) => void close(session)}
      />
      <section className="terminal-workspace">
        <TerminalToolbar
          state={attachment.state}
          onToggleInput={() => void attachment.toggleInputLease()}
          onToggleResizeWithWindow={() =>
            void attachment.toggleResizeWithWindow()
          }
          onReconnect={() => void attachment.reconnect()}
        />
        <div className="terminal-notices">
          {attachment.state.history_gap ? (
            <div className="history-gap-banner" role="status">
              Earlier remote output is no longer contiguous. The live screen was restored from a
              checkpoint.
            </div>
          ) : null}
          {attachment.state.message ? (
            <div className="message-banner" role="status">{attachment.state.message}</div>
          ) : null}
        </div>
        <TerminalSurface
          phase={attachment.state.phase}
          hasSession={attachment.state.session !== null}
          onInput={attachment.handleInput}
          onReady={setRenderer}
        />
        <StatusBar state={attachment.state} />
      </section>
    </main>
  );
}
