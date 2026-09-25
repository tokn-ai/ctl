import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ComponentProps, ReactNode } from "react";
import { createPortal } from "react-dom";
import { useAttachment } from "../../features/attachment/useAttachment";
import { adjacentPane, swapPanes, viewDividers } from "../../features/terminal/viewLayout";
import type { XtermRenderer } from "../../features/terminal/XtermRenderer";
import { sessionKey } from "../../features/targets/targets";
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
  renderer?: XtermRenderer | null;
  input_owned?: boolean;
  on_toggle_input?(): void;
  on_select_terminal(session: SessionSummary): Promise<void>;
  on_promoted(session: SessionSummary): Promise<void>;
  prefix_settings?: { document: KeybindingsDocument; bindings: ReadonlyMap<string, Keybinding>; platform: ShortcutPlatform };
  shortcuts_enabled?: boolean;
  on_command?(id: string): void;
  on_pane_commands?(commands: AppCommand[]): void;
}

export function SessionViewSurface({ session, shell_state, renderer, input_owned, on_toggle_input, on_select_terminal, on_promoted, prefix_settings, shortcuts_enabled = true, on_command, on_pane_commands, ...surface }: Props) {
  const [viewport, setViewport] = useState<HTMLDivElement | null>(null);
  const [controls_host, setControlsHost] = useState<HTMLDivElement | null>(null);
  const [cell, setCell] = useState({ width: 8, height: 16 });
  useLayoutEffect(() => {
    renderer?.setViewport(viewport);
    if (!renderer || !viewport) return;
    const stop = renderer.observeCellDimensions((next) => {
      setCell((previous) => previous.width === next.width && previous.height === next.height ? previous : next);
    });
    return () => { stop(); renderer.setViewport(null); };
  }, [renderer, viewport]);
  const [focused_id, setFocusedId] = useState<string | null>(null);
  const [zoomed_id, setZoomedId] = useState<string | null>(null);
  const pane_elements = useRef(new Map<string, HTMLDivElement>());
  const pane_toggle_inputs = useRef(new Map<string, () => Promise<void>>());
  const pane_inputs = useRef(new Map<string, (data: Uint8Array) => void>());
  const [view, setView] = useState<SessionView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [action_error, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const busy_ref = useRef(false);
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

  const primary_size = `${session?.terminal_size.columns}:${session?.terminal_size.rows}`;
  const previous_size = useRef(primary_size);
  useEffect(() => {
    const changed = previous_size.current !== primary_size;
    previous_size.current = primary_size;
    if (!changed || !connected || busy_ref.current || !session) return;
    const current = generation.current;
    const request = ++sequence.current;
    void sessionView(session.target, { kind: "get", session_id: session.session_id }).then((next) => {
      if (generation.current === current && sequence.current === request) setView(next);
    }).catch(() => { /* The periodic refresh reports connectivity errors. */ });
  }, [primary_size, connected, key]);

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

  const current_view = view?.session_id === session?.session_id ? view : null;
  const primary_id = session?.terminal_id;
  const panes = current_view?.panes.map((pane) => ({ terminal_id: pane.terminal_id, left: pane.left, top: pane.top, width: pane.columns, height: pane.rows, visible: true })) ?? [];
  const focused = panes.some((pane) => pane.terminal_id === focused_id && pane.visible) ? focused_id! : panes.find((pane) => pane.visible)?.terminal_id ?? primary_id;
  const can_split = connected && Boolean(current_view && focused) && !busy;
  const split_focused = useRef<(axis: "horizontal" | "vertical") => Promise<void>>(async () => {});
  split_focused.current = async (axis) => {
    if (!can_split || !focused || !session) return;
    await mutate({ kind: "split", terminal_id: focused, axis, terminal_size: session.terminal_size, working_directory: null });
    requestAnimationFrame(() => pane_elements.current.get(focused)?.querySelector<HTMLTextAreaElement>("textarea")?.focus());
  };
  const toggle_focused = useRef<() => void>(() => {});
  toggle_focused.current = () => {
    if (focused === primary_id) on_toggle_input?.();
    else if (focused) void pane_toggle_inputs.current.get(focused)?.();
    requestAnimationFrame(() => focused && pane_elements.current.get(focused)?.querySelector<HTMLTextAreaElement>("textarea")?.focus());
  };
  useEffect(() => {
    on_pane_commands?.([
      { id: "terminal.toggle_input", category: "Pane", title: "Toggle pane input", enabled: can_split, focusTerminalAfterRun: false, run: () => toggle_focused.current() },
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
  const paneStyle = (rect: typeof primary_rect) => rect ? {
    left: active_zoom ? 0 : rect.left * cell.width,
    top: active_zoom ? 0 : rect.top * cell.height,
    width: rect.width * cell.width, height: rect.height * cell.height,
    visibility: rect.visible && (!active_zoom || rect.terminal_id === active_zoom) ? "visible" as const : "hidden" as const,
  } : { inset: 0 };

  function controls(terminal_id: string, state?: ShellStateSummary | null, input_control?: ReactNode) {
    const label = terminalPaneTitle(state);
    return <div className="view-pane-toolbar">
      <span title={label}>{label}</span>
      {input_control}
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
    {error && <div className="message-banner" role="status">{error}</div>}
    {action_error && <div className="message-banner" role="alert">{action_error}</div>}
    <div className="view-controls" ref={setControlsHost}>
      {connected && primary_id && focused === primary_id && controls(primary_id, shell_state,
        <button onClick={on_toggle_input}>{input_owned ? "Release input" : "Take input"}</button>)}
    </div>
    <div className="view-viewport" ref={setViewport}>
    <div className="view-panes" style={current_view ? { width: current_view.canvas_size.columns * cell.width, height: current_view.canvas_size.rows * cell.height } : { width: "100%", height: "100%" }}>
      {current_view && !active_zoom && viewDividers(current_view.layout, panes).map((divider) => <div
        key={divider.path} className="view-divider" aria-hidden="true"
        style={{ left: Math.round(divider.left * cell.width), top: Math.round(divider.top * cell.height), width: divider.vertical ? 1 : divider.length * cell.width, height: divider.vertical ? divider.length * cell.height : 1 }}
      />)}
      <div className="view-pane" data-active={focused === primary_id} ref={paneRef(primary_id)} onFocusCapture={() => setFocusedId(primary_id ?? null)} style={{ ...paneStyle(primary_rect), ...(takeover_id ? { visibility: "hidden" } : {}) }}>
        <TerminalSurface {...surface} />
      </div>
      {connected && session && current_view && panes.filter((pane) => pane.terminal_id !== primary_id && pane.terminal_id !== takeover_id).map((pane) => {
        const terminal = current_view.terminals.find((candidate) => candidate.terminal_id === pane.terminal_id)!;
        return <div className="view-pane" data-active={focused === pane.terminal_id} key={pane.terminal_id} ref={paneRef(pane.terminal_id)} onFocusCapture={() => setFocusedId(pane.terminal_id)} style={paneStyle(pane)}>
          <AdditionalTerminal session={{ ...session, ...terminal, view_id: current_view.view_id }} detach_registry={pane_detachers} input_registry={pane_inputs} toggle_registry={pane_toggle_inputs} render_controls={(state, input_control) => focused === pane.terminal_id && controls_host ? createPortal(controls(pane.terminal_id, state, input_control), controls_host) : null} />
        </div>;
      })}
    </div>
    </div>
  </div>;
}

function AdditionalTerminal({ session, detach_registry, input_registry, toggle_registry, render_controls }: {
  session: SessionSummary;
  toggle_registry: { current: Map<string, () => Promise<void>> };
  render_controls(state: ShellStateSummary | null, input_control: ReactNode): ReactNode;
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
    toggle_registry.current.set(terminal_id, () => actions.current.toggleInputLease());
    input_registry.current.set(terminal_id, (data) => actions.current.handleInput(data));
    void actions.current.connect(session_ref.current, { resize_with_window: false, terminal_id });
    return () => {
      if (detach_registry.current.get(terminal_id) === detach) detach_registry.current.delete(terminal_id);
      input_registry.current.delete(terminal_id);
      toggle_registry.current.delete(terminal_id);
      void detach();
    };
  }, [renderer, key, detach_registry, input_registry, toggle_registry]);
  return <>
    {render_controls(attachment.state.shell_state, <button onClick={() => void attachment.toggleInputLease()}>{attachment.state.input_lease.owned_by_client ? "Release input" : "Take input"}</button>)}
    {attachment.state.message && <div role="status" className="pane-message">{attachment.state.message}</div>}
    <TerminalSurface phase={attachment.state.phase} hasSession={true} has_cached_content={attachment.state.applied_sequence !== null} onInput={attachment.handleInput} onReady={setRenderer} />
  </>;
}
