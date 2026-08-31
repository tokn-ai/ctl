import { useCallback, useEffect, useState } from "react";
import { SessionSidebar } from "../components/sessions/SessionSidebar";
import { StatusBar } from "../components/status/StatusBar";
import { TerminalSurface } from "../components/terminal/TerminalSurface";
import { TerminalToolbar } from "../components/terminal/TerminalToolbar";
import { useAttachment } from "../features/attachment/useAttachment";
import type { XtermRenderer } from "../features/terminal/XtermRenderer";
import { createSession, errorMessage, listSessions } from "../lib/tauri";
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

  const refresh = useCallback(async () => {
    setLoading(true);
    setListError(null);
    try {
      setSessions(await listSessions());
    } catch (error) {
      setListError(errorMessage(error));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const create = useCallback(
    async (name: string | null, workingDirectory: string | null) => {
      setCreating(true);
      setListError(null);
      try {
        const session = await createSession({
          name,
          working_directory: workingDirectory,
          terminal_size: measuredSize(renderer),
        });
        setSessions((current) => [
          session,
          ...current.filter((item) => item.session_id !== session.session_id),
        ]);
        await attachment.connect(session, { resize_with_window: true });
      } catch (error) {
        setListError(errorMessage(error));
      } finally {
        setCreating(false);
      }
    },
    [attachment, renderer],
  );

  return (
    <main className="app-shell">
      <SessionSidebar
        sessions={sessions}
        activeSessionId={attachment.state.session?.session_id ?? null}
        loading={loading}
        error={listError}
        creating={creating}
        onRefresh={() => void refresh()}
        onSelect={(session) => void attachment.connect(session)}
        onCreate={(name, workingDirectory) => void create(name, workingDirectory)}
      />
      <section className="terminal-workspace">
        <TerminalToolbar
          state={attachment.state}
          onToggleInput={() => void attachment.toggleInputLease()}
          onToggleResizeWithWindow={() =>
            void attachment.toggleResizeWithWindow()
          }
          onReconnect={() => void attachment.reconnect()}
          onDetach={() => void attachment.detach()}
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
