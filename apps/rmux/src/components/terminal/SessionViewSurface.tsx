import { useEffect, useRef, useState } from "react";
import type { ComponentProps, ReactNode } from "react";
import { useAttachment } from "../../features/attachment/useAttachment";
import { adjacentPane, swapPanes, viewPanes, viewTabs } from "../../features/terminal/viewLayout";
import type { XtermRenderer } from "../../features/terminal/XtermRenderer";
import { sameTarget, sessionKey } from "../../features/targets/targets";
import { errorMessage } from "../../lib/errors";
import { sessionView } from "../../lib/tauri";
import type { SessionSummary, SessionView, ShellStateSummary, ViewAction } from "../../lib/types";
import { terminalPaneTitle } from "../../lib/shellState";
import { TerminalSurface } from "./TerminalSurface";
import "./sessionView.css";
import { resolvePrefix, prefixActionMode, PREFIX_ACTIONS } from "../../features/commands/prefixKeymap";
import { useTerminalPrefix } from "../../features/commands/useTerminalPrefix";
import type { KeybindingsDocument } from "../../lib/types";
import type { AppCommand, Keybinding, ShortcutPlatform } from "../../features/commands/types";

type SurfaceProps = ComponentProps<typeof TerminalSurface>;
interface Props extends SurfaceProps {
  session: SessionSummary | null;
  shell_state?: ShellStateSummary | null;
  on_select_terminal(session: SessionSummary): Promise<void>;
  available_sessions: SessionSummary[];
  on_promoted(session: SessionSummary): Promise<void>;
  on_merged(source: SessionSummary): Promise<void>;
  prefix_settings?: { document: KeybindingsDocument; bindings: ReadonlyMap<string, Keybinding>; platform: ShortcutPlatform };
  shortcuts_enabled?: boolean;
  on_command?(id: string): void;
  on_pane_commands?(commands: AppCommand[]): void;
}

export function SessionViewSurface({ session, shell_state, on_select_terminal, available_sessions, on_promoted, on_merged, prefix_settings, shortcuts_enabled = true, on_command, on_pane_commands, ...surface }: Props) {
  const [focused_id, setFocusedId] = useState<string | null>(null);
  const [zoomed_id, setZoomedId] = useState<string | null>(null);
  const pane_elements = useRef(new Map<string, HTMLDivElement>());
  const pane_inputs = useRef(new Map<string, (data: Uint8Array) => void>());
  const [view, setView] = useState<SessionView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [action_error, setActionError] = useState<string | null>(null);
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
    setActionError(null);
    setSelectedTabs({});
    setFocusedId(null);
    setZoomedId(null);
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
    setActionError(null);
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
        // Both toolbar and prefix splits must reveal the newly created pane.
        if (action.kind === "split") setZoomedId(null);
        if (next?.session_id === session.session_id) setView(next);
        else {
          const refreshed = await sessionView(session.target, { kind: "get", session_id: session.session_id });
          if (current === generation.current) setView(refreshed);
        }
      }
    } catch (failure) {
      if (current === generation.current) setActionError(errorMessage(failure));
    } finally {
      busy_ref.current = false;
      setBusy(false);
    }
  }

  const merge_candidates = session ? available_sessions.filter((candidate) => candidate.session_id !== session.session_id && sameTarget(candidate.target, session.target) && candidate.status === "running") : [];
  const current_view = view?.session_id === session?.session_id ? view : null;
  const primary_id = session?.terminal_id;
  const panes = current_view ? viewPanes(current_view.layout, selected_tabs) : [];
  const focused = panes.some((pane) => pane.terminal_id === focused_id && pane.visible) ? focused_id! : panes.find((pane) => pane.visible)?.terminal_id ?? primary_id;
  const can_split = connected && Boolean(current_view && focused) && !busy;
  const split_focused = useRef<(axis: "horizontal" | "vertical") => Promise<void>>(async () => {});
  split_focused.current = async (axis) => {
    if (!can_split || !focused || !session) return;
    await mutate({ kind: "split", terminal_id: focused, axis, terminal_size: session.terminal_size, working_directory: null });
    requestAnimationFrame(() => pane_elements.current.get(focused)?.querySelector<HTMLTextAreaElement>("textarea")?.focus());
  };
  useEffect(() => {
    on_pane_commands?.([
      { id: "pane.split_right", category: "Pane", title: "Split pane right", keywords: ["split right", "horizontal"], enabled: can_split, focusTerminalAfterRun: false, run: () => split_focused.current("horizontal") },
      { id: "pane.split_below", category: "Pane", title: "Split pane below", keywords: ["split below", "vertical"], enabled: can_split, focusTerminalAfterRun: false, run: () => split_focused.current("vertical") },
    ]);
  }, [on_pane_commands, can_split]);
  useEffect(() => () => on_pane_commands?.([]), [on_pane_commands]);
  const prefix_map = resolvePrefix(prefix_settings?.document ?? { schema_version: 1, overrides: [], prefix: { key: null, bindings: [] } }, prefix_settings?.bindings ?? new Map(), prefix_settings?.platform ?? "other");
  const prefix = useTerminalPrefix({
    enabled: shortcuts_enabled && connected && Boolean(current_view),
    context: `${key}:${focused ?? ""}`,
    keymap: prefix_map,
    on_input: (data) => {
      if (focused === primary_id) surface.onInput(data);
      else if (focused) pane_inputs.current.get(focused)?.(data);
    },
    on_action: (action) => {
      if (!focused || !current_view) return;
      if (action.startsWith("pane.focus_") || action.startsWith("pane.move_")) {
        const neighbor = adjacentPane(panes, focused, action.slice(action.lastIndexOf("_") + 1));
        if (!neighbor) return;
        if (action.startsWith("pane.move_")) {
          void mutate({ kind: "update", session_id: session!.session_id, expected_revision: current_view.revision, layout: swapPanes(current_view.layout, focused, neighbor) });
        } else {
          setZoomedId(null);
          setFocusedId(neighbor);
          requestAnimationFrame(() => pane_elements.current.get(neighbor)?.querySelector<HTMLTextAreaElement>("textarea")?.focus());
        }
      } else if (action === "pane.zoom") setZoomedId((previous) => previous === focused ? null : focused);
      else if (action === "pane.split_right" || action === "pane.split_below") {
        void split_focused.current(action === "pane.split_right" ? "horizontal" : "vertical");
      } else if (action === "pane.promote") void mutate({ kind: "promote", terminal_id: focused, name: null });
      else on_command?.(action);
    },
  });
  const paneRef = (id: string | undefined) => (element: HTMLDivElement | null) => {
    if (!id) return;
    if (element) pane_elements.current.set(id, element);
    else pane_elements.current.delete(id);
  };
  const primary_rect = panes.find((pane) => pane.terminal_id === primary_id);
  const takeover_id = current_view && primary_id && !primary_rect
    ? current_view.terminals[0]?.terminal_id
    : null;
  const active_zoom = panes.some((pane) => pane.terminal_id === zoomed_id && pane.visible) ? zoomed_id : null;
  const paneStyle = (rect: typeof primary_rect) => rect && active_zoom ? { inset: 0, visibility: rect.terminal_id === active_zoom ? "visible" as const : "hidden" as const } : rect ? {
    left: `${rect.left}%`, top: `${rect.top}%`, width: `${rect.width}%`, height: `${rect.height}%`,
    visibility: rect.visible ? "visible" as const : "hidden" as const,
  } : { inset: 0 };

  function controls(terminal_id: string, state?: ShellStateSummary | null) {
    const label = terminalPaneTitle(state);
    return <div className="view-pane-toolbar">
      <span title={label}>{label}</span>
      <button disabled={busy} title="Split side by side" onClick={() => void mutate({ kind: "split", terminal_id, axis: "horizontal", terminal_size: session!.terminal_size, working_directory: null })}>Split right</button>
      <button disabled={busy} title="Split vertically" onClick={() => void mutate({ kind: "split", terminal_id, axis: "vertical", terminal_size: session!.terminal_size, working_directory: null })}>Split below</button>
      <button disabled={busy} onClick={() => void mutate({ kind: "kill_terminal", terminal_id })}>Terminate pane</button>
      {panes.length > 1 && <button disabled={busy} onClick={() => void mutate({ kind: "promote", terminal_id, name: null })}>Move to new session</button>}
    </div>;
  }

  return <div className="session-view" onKeyDownCapture={prefix.onKeyDown} onBlurCapture={(event) => {
    if (!(event.relatedTarget instanceof Element) || !event.currentTarget.contains(event.relatedTarget) || !event.relatedTarget.closest(".terminal-container")) prefix.cancel();
  }}>
    {prefix.mode && <div className="terminal-prefix-hints" role="status" aria-live="polite">
      <strong>{prefix_map.label}{prefix.mode === "move" ? " · Move pane" : ""}</strong>
      {PREFIX_ACTIONS.filter((action) => prefixActionMode(action.id) === prefix.mode && prefix_map.bindings.has(action.id)).map((action) => <span key={action.id}><kbd>{prefix_map.bindings.get(action.id)}</kbd> {action.title}</span>)}
      <span>{prefix.mode === "move" ? "Enter or Esc finishes" : `${prefix_map.label} again sends the prefix · Esc cancels`}</span>
    </div>}
    {connected && merge_candidates.length > 0 && <div className="view-tabs">
      <select aria-label="Session to merge" value={merge_source} onChange={(event) => setMergeSource(event.target.value)}>
        <option value="">Merge another session…</option>
        {merge_candidates.map((candidate) => <option key={candidate.session_id} value={candidate.session_id}>{candidate.name}</option>)}
      </select>
      <button disabled={busy || !merge_candidates.some((candidate) => candidate.session_id === merge_source)} onClick={() => void mutate({ kind: "merge", source: merge_source, destination: session!.session_id })}>Merge into this session</button>
    </div>}
    {error && <div className="message-banner" role="status">{error}</div>}
    {action_error && <div className="message-banner" role="alert">{action_error}</div>}
    {current_view && viewTabs(current_view.layout).map((group) => <div className="view-tabs" key={group.path} role="tablist" aria-label="Terminal group">
      {Array.from({ length: group.count }, (_, index) => <button key={index} role="tab" aria-selected={(selected_tabs[group.path] ?? 0) === index} onClick={() => setSelectedTabs((previous) => ({ ...previous, [group.path]: index }))}>Group {index + 1}</button>)}
    </div>)}
    <div className="view-panes">
      <div className="view-pane" ref={paneRef(primary_id)} onFocusCapture={() => setFocusedId(primary_id ?? null)} style={{ ...paneStyle(primary_rect), ...(takeover_id ? { visibility: "hidden" } : {}) }}>
        {connected && primary_id && controls(primary_id, shell_state)}
        <TerminalSurface {...surface} />
      </div>
      {connected && session && current_view && panes.filter((pane) => pane.terminal_id !== primary_id && pane.terminal_id !== takeover_id).map((pane) => {
        const terminal = current_view.terminals.find((candidate) => candidate.terminal_id === pane.terminal_id)!;
        return <div className="view-pane" key={pane.terminal_id} ref={paneRef(pane.terminal_id)} onFocusCapture={() => setFocusedId(pane.terminal_id)} style={paneStyle(pane)}>
          <AdditionalTerminal session={{ ...session, ...terminal, view_id: current_view.view_id }} detach_registry={pane_detachers} input_registry={pane_inputs} render_controls={(state) => controls(pane.terminal_id, state)} />
        </div>;
      })}
    </div>
  </div>;
}

function AdditionalTerminal({ session, detach_registry, input_registry, render_controls }: {
  session: SessionSummary;
  render_controls(state: ShellStateSummary | null): ReactNode;
  detach_registry: { current: Map<string, () => Promise<void>> };
  input_registry: { current: Map<string, (data: Uint8Array) => void> };
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
    input_registry.current.set(terminal_id, (data) => actions.current.handleInput(data));
    void actions.current.connect(session_ref.current, { resize_with_window: true, terminal_id });
    return () => {
      if (detach_registry.current.get(terminal_id) === detach) detach_registry.current.delete(terminal_id);
      input_registry.current.delete(terminal_id);
      void detach();
    };
  }, [renderer, key, detach_registry, input_registry]);
  return <>
    {render_controls(attachment.state.shell_state)}
    {attachment.state.message && <div role="status" className="message-banner">{attachment.state.message}</div>}
    <div className="view-pane-leases">
      <button onClick={() => void attachment.toggleInputLease()}>{attachment.state.input_lease.owned_by_client ? "Release input" : "Take input"}</button>
      <button onClick={() => void attachment.toggleResizeWithWindow()}>{attachment.state.resize_with_window ? "Stop resizing" : "Resize to pane"}</button>
    </div>
    <TerminalSurface phase={attachment.state.phase} hasSession={true} has_cached_content={attachment.state.applied_sequence !== null} onInput={attachment.handleInput} onReady={setRenderer} />
  </>;
}
