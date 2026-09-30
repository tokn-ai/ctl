import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import type { AppNotification, NotificationAction } from "../../lib/types";
import type { NotificationStore } from "../../features/notifications/NotificationStore";
import { useCommandEnvironment, useCommandScope, useEffectiveCommands } from "../../features/commands/CommandContext";
import { QUICK_INPUT_IDS } from "../../features/commands/commandIds";
import "./notifications.css";

export function NotificationIcon({ kind }: { kind: string }) {
  return <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {kind === "bell" ? <><path d="M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9" /><path d="M10 21h4" /></>
      : kind === "close" ? <path d="m6 6 12 12M6 18 18 6" />
      : kind === "hide" ? <path d="m6 9 6 6 6-6" />
      : kind === "clear" ? <><path d="m4 7 2 2 4-4m-6 9 2 2 4-4m2-5h8m-8 7h8M4 21h16" /></>
      : kind === "success" ? <><circle cx="12" cy="12" r="9" /><path d="m8 12 3 3 5-6" /></>
      : kind === "warning" ? <><path d="m12 3 10 18H2L12 3Z" /><path d="M12 9v5m0 3v.1" /></>
      : <><circle cx="12" cy="12" r="9" /><path d={kind === "error" ? "m9 9 6 6m-6 0 6-6" : "M12 11v6m0-10v.1"} /></>}
  </svg>;
}

export function NotificationBell({ store, disabled = false }: { store: NotificationStore; disabled?: boolean }) {
  const state = useSyncExternalStore(store.subscribe, store.snapshot);
  const unread = state.entries.filter((entry) => !entry.read).length;
  return <button
    type="button"
    id="notification-bell"
    className={`notification-bell ${state.center_open ? "active" : ""}`}
    aria-label={`Notifications${unread ? `, ${unread} unread` : ""}`}
    title={state.center_open ? "Hide notification center" : "Show notification center"}
    aria-expanded={state.center_open}
    aria-controls={state.center_open ? "notification-center" : undefined}
    disabled={disabled}
    onClick={() => store.setCenterOpen(!state.center_open)}
  >
    <NotificationIcon kind="bell" />
    {unread > 0 ? <span className="notification-badge" aria-hidden="true">{unread > 99 ? "99+" : unread}</span> : null}
  </button>;
}

function NotificationCard({ entry, store, toast, execute, canExecute }: {
  entry: AppNotification;
  store: NotificationStore;
  toast: boolean;
  execute(action: NotificationAction, id: string): void;
  canExecute(action: NotificationAction): boolean;
}) {
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [visible, setVisible] = useState(document.visibilityState !== "hidden");
  const remaining = useRef(8000);
  const remove = (button: HTMLButtonElement, hide: boolean) => {
    if (document.activeElement === button) {
      document.getElementById(toast ? "notification-bell" : "notification-center")?.focus();
    }
    if (hide) store.hide(entry.id);
    else store.dismiss(entry.id);
  };
  useEffect(() => {
    if (!toast) return;
    const visibility = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", visibility);
    return () => document.removeEventListener("visibilitychange", visibility);
  }, [toast]);
  useEffect(() => { remaining.current = 8000; }, [entry.updated_at]);
  useEffect(() => {
    if (!toast || hovered || focused || !visible || !["info", "success"].includes(entry.severity)) return;
    const started = Date.now();
    const timer = setTimeout(() => store.hide(entry.id), remaining.current);
    return () => {
      clearTimeout(timer);
      remaining.current = Math.max(0, remaining.current - (Date.now() - started));
    };
  }, [toast, hovered, focused, visible, entry.id, entry.severity, entry.updated_at, store]);
  return <article
    className={`notification-card notification-${entry.severity}`}
    aria-label={entry.title}
    onMouseEnter={() => setHovered(true)}
    onMouseLeave={() => setHovered(false)}
    onFocus={() => setFocused(true)}
    onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget)) setFocused(false); }}
  >
    <div className="notification-card-heading">
      <span className="notification-severity" title={entry.severity}><NotificationIcon kind={entry.severity} /></span>
      <strong>{entry.title}</strong>
      <div className="notification-controls">
        {toast ? <button type="button" aria-label={`Hide ${entry.title} notification`} title="Hide card; keep in notification center" onClick={(event) => remove(event.currentTarget, true)}><NotificationIcon kind="hide" /></button> : null}
        <button type="button" aria-label={`Dismiss ${entry.title} notification`} title="Dismiss notification" onClick={(event) => remove(event.currentTarget, false)}><NotificationIcon kind="close" /></button>
      </div>
    </div>
    <p className="notification-message">{entry.message}</p>
    <div className="notification-meta"><span title={entry.source}>{entry.source}</span><time dateTime={new Date(entry.updated_at).toISOString()} title={new Date(entry.updated_at).toLocaleString()}>{new Date(entry.updated_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time>{entry.occurrence_count > 1 ? <span title="Occurrences">×{entry.occurrence_count}</span> : null}</div>
    {entry.actions?.length ? <div className="notification-actions">{entry.actions.map((action) => <button type="button" key={`${action.command_id}:${action.label}`} disabled={!canExecute(action)} onClick={() => execute(action, entry.id)}>{action.label}</button>)}</div> : null}
  </article>;
}

function NotificationCenter({ store, children, count }: { store: NotificationStore; children: ReactNode; count: number }) {
  const panel = useRef<HTMLElement>(null);
  const dismiss = () => store.setCenterOpen(false);
  const scope = useMemo(() => ({ allow_app_commands: true, commands: [{
    id: QUICK_INPUT_IDS.cancel, title: "Hide Notification Center", category: "Notifications", enabled: true,
    run: () => store.setCenterOpen(false),
  }] }), [store]);
  useCommandScope(scope);
  useEffect(() => {
    const previous = document.activeElement;
    const current = panel.current;
    current?.focus();
    return () => {
      if ((current?.contains(document.activeElement) || document.activeElement === document.body) && previous instanceof HTMLElement && previous.isConnected) previous.focus();
    };
  }, []);
  return <section id="notification-center" className="notification-center" aria-label="Notification center" ref={panel} tabIndex={-1}>
    <header className="notification-center-heading"><strong>Notifications <span>{count}</span></strong><button type="button" aria-label="Dismiss all notifications" title="Dismiss all notifications" disabled={count === 0} onClick={() => store.clear()}><NotificationIcon kind="clear" /></button><button type="button" aria-label="Hide notification center" title="Hide notification center" onClick={dismiss}><NotificationIcon kind="hide" /></button></header>
    <div className="notification-history">{count ? children : <div className="notification-empty"><NotificationIcon kind="bell" /><strong>No notifications</strong><p>New notifications will appear here.</p></div>}</div>
  </section>;
}

export function Notifications({ store, blocked }: { store: NotificationStore; blocked: boolean }) {
  const state = useSyncExternalStore(store.subscribe, store.snapshot);
  const environment = useCommandEnvironment()!;
  useEffectiveCommands(environment.dispatcher);
  useEffect(() => {
    if (blocked && store.snapshot().center_open) store.setCenterOpen(false);
  }, [blocked, store]);
  if (blocked) return null;
  const entries = state.center_open ? state.entries : state.entries.filter((entry) => entry.toast_visible);
  const cards = entries.map((entry) => <NotificationCard key={entry.id} entry={entry} store={store} toast={!state.center_open}
    canExecute={(action) => environment.dispatcher.canExecute(action.command_id, action.args)}
    execute={(action, id) => {
      if (environment.dispatcher.execute(action.command_id, action.args)) {
        store.hide(id);
        store.setCenterOpen(false);
      }
    }}
  />);
  return state.center_open
    ? <NotificationCenter store={store} count={state.entries.length}>{cards}</NotificationCenter>
    : <div className="notification-toasts" role="log" aria-label="Notifications" aria-live="polite" aria-relevant="additions text">{cards}</div>;
}
