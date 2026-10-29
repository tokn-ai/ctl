import { useCallback, useEffect, useRef, useState } from "react";
import { sessionView, sshConnectionStatus } from "../../lib/tauri";
import type { HostConnectionStatus, SessionSummary, SessionView, ViewLayout } from "../../lib/types";
import { subscribeSessionViews } from "../attachment/componentActions";
import { sameTarget, sessionKey } from "../targets/targets";
import { sameSshEndpoint } from "../workspace/remoteRecovery";
import { observedAt } from "./sessionObservation";

interface Options {
  sessions: readonly SessionSummary[];
  host_connections: ReadonlyMap<string, HostConnectionStatus>;
  enabled: boolean;
  /** Existing attachments supply views and must not turn heartbeats into lookups. */
  attached_session_keys?: ReadonlySet<string>;
}

interface Source {
  session: SessionSummary;
  observed_at: number | null;
  requestable: boolean;
  attempted: boolean;
}

interface Observation {
  session: SessionSummary;
  view: SessionView;
}

const MAX_CONCURRENT_LOOKUPS = 4;

function sameSource(left: SessionSummary, right: SessionSummary): boolean {
  return left.session_id === right.session_id && sameTarget(left.target, right.target) &&
    sameSshEndpoint(left.target, right.target) && left.view_id === right.view_id &&
    (left.target.kind !== "ssh" || right.target.kind !== "ssh" ||
      left.target.remote_info?.remote_id === right.target.remote_info?.remote_id);
}

function observationMatches(entry: Observation, session: SessionSummary): boolean {
  // An attachment can publish its first view before its session summary is
  // adopted. Keep that observation through adoption, but never restore a known
  // view after the source loses or replaces its runtime identity.
  return sameSource(entry.session, { ...session, view_id: entry.session.view_id }) &&
    (session.view_id ? session.view_id === entry.view.view_id : !entry.session.view_id);
}

function canRequest(session: SessionSummary, options: Options): boolean {
  if (!options.enabled || options.attached_session_keys?.has(sessionKey(session)) ||
    !session.view_id || observedAt(session) === null ||
    (session.status !== "running" && session.status !== "exited")) return false;
  if (session.target.kind === "local") return true;
  const status = options.host_connections.get(session.target.host_id ?? "");
  return Boolean(session.target.remote_info?.remote_id && !session.target.unavailable &&
    status?.state === "connected" && !status.manually_disconnected &&
    !status.observation?.stale &&
    (!status.observation || status.observation.availability === "available"));
}

/** Runtime presentation metadata only; restored sessions never initiate a lookup. */
export function useSessionLayouts(options: Options) {
  const mounted = useRef(false);
  const enabled = useRef(options.enabled);
  enabled.current = options.enabled;
  const sources = useRef(new Map<string, Source>());
  const previous = sources.current;
  sources.current = new Map(options.sessions.map((session) => {
    const key = sessionKey(session);
    const old = previous.get(key);
    const observed_at = observedAt(session);
    const requestable = canRequest(session, options);
    const source = old && sameSource(old.session, session) && old.observed_at === observed_at &&
      old.requestable === requestable ? old : { session, observed_at, requestable, attempted: false };
    return [key, source];
  }));
  const [observations, setObservations] = useState<ReadonlyMap<string, Observation>>(new Map());
  const running = useRef(0);
  const pumping = useRef<() => void>(() => undefined);

  const observeView = useCallback((session: SessionSummary, view: SessionView) => {
    if (!mounted.current || !enabled.current || view.session_id !== session.session_id ||
      !/^\d+$/.test(view.revision) || session.view_id && session.view_id !== view.view_id) return;
    const key = sessionKey(session);
    const currentSource = () => {
      const current = sources.current.get(key)?.session;
      const valid = current && sameTarget(current.target, session.target) &&
        sameSshEndpoint(current.target, session.target) &&
        (!current.view_id || current.view_id === view.view_id) &&
        (current.target.kind !== "ssh" || session.target.kind !== "ssh" ||
          current.target.remote_info?.remote_id === session.target.remote_info?.remote_id);
      return valid ? current : undefined;
    };
    if (!currentSource()) return;
    setObservations((current) => {
      const source = currentSource();
      if (!source) return current;
      const cached = current.get(key);
      if (cached && observationMatches(cached, source) && cached.view.view_id === view.view_id &&
        BigInt(cached.view.revision) >= BigInt(view.revision)) return current;
      return new Map(current).set(key, { session: source, view });
    });
  }, []);

  pumping.current = () => {
    if (!mounted.current || !enabled.current) return;
    for (const [key, source] of sources.current) {
      if (running.current >= MAX_CONCURRENT_LOOKUPS) break;
      if (!source.requestable || source.attempted) continue;
      source.attempted = true;
      running.current++;
      const isCurrent = () => mounted.current && enabled.current && sources.current.get(key) === source;
      void (async () => {
        // A host can have another method connected. Check this exact route
        // without authenticating before opening a supplemental service channel.
        if (source.session.target.kind === "ssh") {
          const connection = await sshConnectionStatus(source.session.target);
          if (!isCurrent() || !connection.connected || connection.manually_disconnected) return;
        }
        const view = await sessionView(source.session.target, { kind: "get", session_id: source.session.session_id });
        if (isCurrent() && view) observeView(source.session, view);
      })().catch(() => {
        // An icon lookup must not change session health or show connection UI.
      }).finally(() => {
        running.current--;
        pumping.current();
      });
    }
  };

  useEffect(() => {
    mounted.current = true;
    const stop = subscribeSessionViews((event) => {
      if (event.view) observeView(event.session, event.view);
    });
    pumping.current();
    return () => { mounted.current = false; stop(); };
  }, [observeView]);

  useEffect(() => {
    pumping.current();
    setObservations((current) => {
      const kept = new Map<string, Observation>();
      let changed = false;
      for (const [key, entry] of current) {
        const session = sources.current.get(key)?.session;
        if (!session || !observationMatches(entry, session)) { changed = true; continue; }
        const adopted = Boolean(session.view_id && !entry.session.view_id);
        kept.set(key, adopted ? { ...entry, session } : entry);
        changed ||= adopted;
      }
      return changed ? kept : current;
    });
  });

  const layouts = new Map<string, ViewLayout>();
  for (const [key, entry] of observations) {
    const session = sources.current.get(key)?.session;
    if (session && observationMatches(entry, session)) layouts.set(key, entry.view.layout);
  }
  return { layouts, observeView };
}
