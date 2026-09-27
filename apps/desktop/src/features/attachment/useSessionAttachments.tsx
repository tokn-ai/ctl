import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { AttachmentViewState, SessionSummary } from "../../lib/types";
import { sessionCache } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import { sessionKey, targetKey } from "../targets/targets";
import { sameSshEndpoint } from "../workspace/remoteRecovery";
import type { XtermRenderer } from "../terminal/XtermRenderer";
import { useAttachment, type AttachmentActions, type ConnectOptions } from "./useAttachment";

interface Entry {
  session: SessionSummary;
  options: ConnectOptions;
  actions?: AttachmentActions;
  ready: Promise<void>;
  publish(): void;
}

function createEntry(session: SessionSummary, options: ConnectOptions): Entry {
  let publish!: () => void;
  const ready = new Promise<void>((resolve) => { publish = resolve; });
  return { session, options, ready, publish };
}

function SessionAttachment({ entry, renderer, onChange }: {
  entry: Entry;
  renderer: XtermRenderer | null;
  onChange(): void;
}) {
  const scoped = useMemo(() => renderer?.sessionRenderer(entry.session) ?? null, [renderer, entry]);
  const actions = useAttachment(scoped, true);
  const actions_ref = useRef(actions);
  actions_ref.current = actions;
  entry.actions = actions;
  useEffect(onChange, [actions.state, entry, onChange]);
  useEffect(() => {
    if (scoped) void actions_ref.current.connect(entry.session, entry.options).finally(entry.publish);
    return entry.publish;
  }, [entry, scoped]);
  return null;
}

/** Open tabs own streams. Selecting a tab only changes the visible renderer. */
export function useSessionAttachments(renderer: XtermRenderer | null): AttachmentActions & {
  controllers: ReactNode;
  states: readonly AttachmentViewState[];
  session_keys: ReadonlySet<string>;
  closeSession(session: SessionSummary): Promise<void>;
  retainSessions(keys: ReadonlySet<string>): void;
  disconnectHost(host_id: string): Promise<void>;
} {
  // The idle attachment supplies the placeholder state and never opens a stream.
  const idle = useAttachment(null, true);
  const entries = useRef(new Map<string, Entry>());
  const selected = useRef<string | null>(null);
  const archiving = useRef(new Set<string>());
  const [storage_error, setStorageError] = useState<string | null>(null);
  const [revision, refresh] = useState(0);
  const notify = useCallback(() => refresh((revision) => revision + 1), []);
  const renderer_ref = useRef(renderer);
  renderer_ref.current = renderer;

  useEffect(() => {
    const entry = entries.current.get(selected.current ?? "");
    if (entry) renderer?.selectSession({ ...entry.session, terminal_id: entry.options.terminal_id });
  }, [renderer]);

  const connect = useCallback(async (session: SessionSummary, options: ConnectOptions = {}) => {
    const key = sessionKey(session);
    setStorageError(null);
    selected.current = key;
    let entry = entries.current.get(key);
    renderer_ref.current?.selectSession({ ...(entry?.session ?? session), terminal_id: entry?.options.terminal_id ?? options.terminal_id });
    notify();
    if (entry) {
      await entry.ready;
      if (entries.current.get(key) !== entry || !entry.actions) return;
      const actions = entry.actions;
      const current = actions.state;
      if (!sameSshEndpoint(entry.session.target, session.target) || (options.terminal_id !== undefined && entry.options.terminal_id !== options.terminal_id) || current.phase === "idle") {
        entry.session = session;
        entry.options = { ...entry.options, ...options };
        notify();
        await actions.connect(session, entry.options);
      } else if (current.phase === "disconnected" || current.phase === "error") {
        notify();
        await actions.reconnect();
      } else {
        notify();
      }
      renderer_ref.current?.focus();
      return;
    }
    entry = createEntry(session, options);
    entries.current.set(key, entry);
    notify();
    await entry.ready;
  }, [notify]);

  const closeSession = useCallback(async (session: SessionSummary) => {
    const key = sessionKey(session);
    const entry = entries.current.get(key);
    entries.current.delete(key);
    if (selected.current === key) selected.current = null;
    notify();
    await entry?.actions?.detach();
    renderer_ref.current?.retainSessions(new Set(entries.current.keys()));
  }, [notify]);

  const retainSessions = useCallback((keys: ReadonlySet<string>) => {
    for (const [key, entry] of entries.current) {
      if (!keys.has(key) && !archiving.current.has(key)) {
        archiving.current.add(key);
        void closeSession(entry.session)
          .then(() => sessionCache({ kind: "archive", host_key: targetKey(entry.session.target), session_id: entry.session.session_id, reason: "Tab closed" }))
          .catch((error) => setStorageError(errorMessage(error)))
          .finally(() => archiving.current.delete(key));
      }
    }
  }, [closeSession]);

  const states = useMemo(() => [...entries.current.values()].flatMap((entry) => entry.actions ? [entry.actions.state] : []), [revision]);
  const session_keys = useMemo(() => new Set(entries.current.keys()), [revision]);
  const active = () => entries.current.get(selected.current ?? "")?.actions;
  const actions = active() ?? idle;
  return {
    state: storage_error ? { ...actions.state, message: storage_error } : actions.state,
    states,
    session_keys,
    connect,
    reconnect: () => active()?.reconnect() ?? Promise.resolve(),
    cancelPendingConnection: (session) => {
      const entry = entries.current.get(sessionKey(session));
      entry?.actions?.cancelPendingConnection(session);
    },
    detach: async () => {
      // Leaving the terminal view does not close any open session.
      selected.current = null;
      notify();
    },
    resetAfterDaemonRestart: () => {
      for (const [key, entry] of entries.current) {
        if (entry.session.target.kind !== "local") continue;
        entry.actions?.resetAfterDaemonRestart();
        entries.current.delete(key);
        if (selected.current === key) selected.current = null;
      }
      notify();
    },
    handleInput: (data) => active()?.handleInput(data),
    toggleInputLease: () => active()?.toggleInputLease() ?? Promise.resolve(),
    toggleResizeWithWindow: () => active()?.toggleResizeWithWindow() ?? Promise.resolve(),
    closeSession,
    retainSessions,
    disconnectHost: async (host_id) => {
      await Promise.all([...entries.current.values()]
        .filter((entry) => entry.session.target.kind === "ssh" && entry.session.target.host_id === host_id)
        .map((entry) => closeSession(entry.session)));
    },
    controllers: [...entries.current].map(([key, entry]) =>
      <SessionAttachment key={key} entry={entry} renderer={renderer} onChange={notify} />),
  };
}
