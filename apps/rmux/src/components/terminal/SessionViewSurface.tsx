import { useEffect, useRef, useState } from "react";
import type { ComponentProps } from "react";
import { useAttachment } from "../../features/attachment/useAttachment";
import { viewPanes, viewTabs } from "../../features/terminal/viewLayout";
import type { XtermRenderer } from "../../features/terminal/XtermRenderer";
import { sameTarget, sessionKey } from "../../features/targets/targets";
import { errorMessage } from "../../lib/errors";
import { sessionView } from "../../lib/tauri";
import type { SessionSummary, SessionView, ViewAction } from "../../lib/types";
import { TerminalSurface } from "./TerminalSurface";
import "./sessionView.css";

type SurfaceProps = ComponentProps<typeof TerminalSurface>;
interface Props extends SurfaceProps {
  session: SessionSummary | null;
  on_select_terminal(session: SessionSummary): Promise<void>;
  available_sessions: SessionSummary[];
  on_promoted(session: SessionSummary): Promise<void>;
  on_merged(source: SessionSummary): Promise<void>;
}

export function SessionViewSurface({ session, on_select_terminal, available_sessions, on_promoted, on_merged, ...surface }: Props) {
  const [view, setView] = useState<SessionView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const busy_ref = useRef(false);
  const [merge_source, setMergeSource] = useState("");
  const [selected_tabs, setSelectedTabs] = useState<Record<string, number>>({});
  const pane_detachers = useRef(new Map<string, () => Promise<void>>());
  const session_ref = useRef(session);
  session_ref.current = session;
  const key = session ? sessionKey(session) : "";
  const connected = surface.phase === "attached" || surface.phase === "ended";
  const generation = useRef(0);
  const sequence = useRef(0);
  const select_ref = useRef(on_select_terminal);
  select_ref.current = on_select_terminal;

  useEffect(() => {
    const current = ++generation.current;
    setView(null);
    setError(null);
    setSelectedTabs({});
    if (!key || !connected) return;
    let timer: ReturnType<typeof setTimeout>;
    async function refresh() {
      const root = session_ref.current;
      if (!root) return;
      if (busy_ref.current) { timer = setTimeout(() => void refresh(), 2000); return; }
      const request_sequence = ++sequence.current;
      try {
        const next = await sessionView(root.target, { kind: "get", session_id: root.session_id });
        if (generation.current !== current || request_sequence !== sequence.current) return;
        setView(next);
        setError(null);
        if (next && root.terminal_id && !next.terminals.some((terminal) => terminal.terminal_id === root.terminal_id)) {
          const first = next.terminals[0];
          if (first) {
            // Release the pane's existing leases before the primary attachment
            // takes over; otherwise the two opens race for input ownership.
            await pane_detachers.current.get(first.terminal_id)?.();
            if (generation.current === current) {
              await select_ref.current({ ...root, ...first, name: root.name, view_id: next.view_id });
            }
          }
        }
      } catch (failure) {
        if (generation.current === current && request_sequence === sequence.current) setError(errorMessage(failure));
      } finally {
        if (generation.current === current) timer = setTimeout(() => void refresh(), 2000);
      }
    }
    void refresh();
    return () => { generation.current++; clearTimeout(timer); };
  }, [key, connected]);

  async function mutate(action: ViewAction) {
    if (!session || busy_ref.current) return;
    busy_ref.current = true;
    const current = generation.current;
    ++sequence.current;
    setBusy(true);
    setError(null);
    try {
      const next = await sessionView(session.target, action);
      if (next && action.kind === "promote") {
        const terminal = next.terminals[0];
        await on_promoted({ ...session, ...terminal, name: next.session_name, session_id: next.session_id, view_id: next.view_id });
      }
      if (next && action.kind === "merge") {
        const source = available_sessions.find((candidate) => candidate.session_id === action.source && sameTarget(candidate.target, session.target));
        if (source) await on_merged(source);
      }
      if (current === generation.current) {
        ++sequence.current;
        if (next?.session_id === session.session_id) setView(next);
        else {
          const refreshed = await sessionView(session.target, { kind: "get", session_id: session.session_id });
          if (current === generation.current) setView(refreshed);
        }
      }
    } catch (failure) {
      if (current === generation.current) setError(errorMessage(failure));
    } finally {
      busy_ref.current = false;
      setBusy(false);
    }
  }

  const merge_candidates = session ? available_sessions.filter((candidate) => candidate.session_id !== session.session_id && sameTarget(candidate.target, session.target) && candidate.status === "running") : [];
  const current_view = view?.session_id === session?.session_id ? view : null;
  const primary_id = session?.terminal_id;
  const panes = current_view ? viewPanes(current_view.layout, selected_tabs) : [];
  const primary_rect = panes.find((pane) => pane.terminal_id === primary_id);
  const takeover_id = current_view && primary_id && !primary_rect
    ? current_view.terminals[0]?.terminal_id
    : null;
  const paneStyle = (rect: typeof primary_rect) => rect ? {
    left: `${rect.left}%`, top: `${rect.top}%`, width: `${rect.width}%`, height: `${rect.height}%`,
    visibility: rect.visible ? "visible" as const : "hidden" as const,
  } : { inset: 0 };

  function controls(terminal_id: string, label: string) {
    return <div className="view-pane-toolbar">
      <span>{label}</span>
      <button disabled={busy} title="Split side by side" onClick={() => void mutate({ kind: "split", terminal_id, axis: "horizontal", terminal_size: session!.terminal_size, working_directory: null })}>Split right</button>
      <button disabled={busy} title="Split vertically" onClick={() => void mutate({ kind: "split", terminal_id, axis: "vertical", terminal_size: session!.terminal_size, working_directory: null })}>Split below</button>
      <button disabled={busy} onClick={() => void mutate({ kind: "kill_terminal", terminal_id })}>Terminate pane</button>
      {panes.length > 1 && <button disabled={busy} onClick={() => void mutate({ kind: "promote", terminal_id, name: null })}>Move to new session</button>}
    </div>;
  }

  return <div className="session-view">
    {connected && merge_candidates.length > 0 && <div className="view-tabs">
      <select aria-label="Session to merge" value={merge_source} onChange={(event) => setMergeSource(event.target.value)}>
        <option value="">Merge another session…</option>
        {merge_candidates.map((candidate) => <option key={candidate.session_id} value={candidate.session_id}>{candidate.name}</option>)}
      </select>
      <button disabled={busy || !merge_candidates.some((candidate) => candidate.session_id === merge_source)} onClick={() => void mutate({ kind: "merge", source: merge_source, destination: session!.session_id })}>Merge into this session</button>
    </div>}
    {error && <div className="message-banner" role="status">{error}</div>}
    {current_view && viewTabs(current_view.layout).map((group) => <div className="view-tabs" key={group.path} role="tablist" aria-label="Terminal group">
      {Array.from({ length: group.count }, (_, index) => <button key={index} role="tab" aria-selected={(selected_tabs[group.path] ?? 0) === index} onClick={() => setSelectedTabs((previous) => ({ ...previous, [group.path]: index }))}>Group {index + 1}</button>)}
    </div>)}
    <div className="view-panes">
      <div className="view-pane" style={{ ...paneStyle(primary_rect), ...(takeover_id ? { visibility: "hidden" } : {}) }}>
        {connected && primary_id && controls(primary_id, session?.name ?? "Terminal")}
        <TerminalSurface {...surface} />
      </div>
      {connected && session && current_view && panes.filter((pane) => pane.terminal_id !== primary_id && pane.terminal_id !== takeover_id).map((pane) => {
        const terminal = current_view.terminals.find((candidate) => candidate.terminal_id === pane.terminal_id)!;
        return <div className="view-pane" key={pane.terminal_id} style={paneStyle(pane)}>
          {controls(pane.terminal_id, terminal.name)}
          <AdditionalTerminal session={{ ...session, ...terminal, view_id: current_view.view_id }} detach_registry={pane_detachers} />
        </div>;
      })}
    </div>
  </div>;
}

function AdditionalTerminal({ session, detach_registry }: {
  session: SessionSummary;
  detach_registry: { current: Map<string, () => Promise<void>> };
}) {
  const [renderer, setRenderer] = useState<XtermRenderer | null>(null);
  const attachment = useAttachment(renderer);
  const actions = useRef(attachment);
  actions.current = attachment;
  const session_ref = useRef(session);
  session_ref.current = session;
  const key = `${sessionKey(session)}:${session.terminal_id}`;
  useEffect(() => {
    if (!renderer) return;
    const terminal_id = session_ref.current.terminal_id!;
    const detach = () => actions.current.detach();
    detach_registry.current.set(terminal_id, detach);
    void actions.current.connect(session_ref.current, { resize_with_window: true, terminal_id });
    return () => {
      if (detach_registry.current.get(terminal_id) === detach) detach_registry.current.delete(terminal_id);
      void detach();
    };
  }, [renderer, key, detach_registry]);
  return <>
    {attachment.state.message && <div role="status" className="message-banner">{attachment.state.message}</div>}
    <div className="view-pane-leases">
      <button onClick={() => void attachment.toggleInputLease()}>{attachment.state.input_lease.owned_by_client ? "Release input" : "Take input"}</button>
      <button onClick={() => void attachment.toggleResizeWithWindow()}>{attachment.state.resize_with_window ? "Stop resizing" : "Resize to pane"}</button>
    </div>
    <TerminalSurface phase={attachment.state.phase} hasSession={true} has_cached_content={attachment.state.applied_sequence !== null} onInput={attachment.handleInput} onReady={setRenderer} />
  </>;
}
