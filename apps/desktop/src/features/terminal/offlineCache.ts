import type { CachedSessionView, CachedTerminalSnapshot, SessionSummary, SessionView } from "../../lib/types";
import { sessionKey } from "../targets/targets";

const STORAGE_KEY = "rmux.offline_views.v1";
const MAX_BYTES = 2_000_000;
const MAX_AGE = 7 * 24 * 60 * 60 * 1000;
interface Cache {
  panes: Record<string, CachedTerminalSnapshot>;
  views: Record<string, CachedSessionView>;
}

function read(): Cache {
  try {
    const value = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null");
    if (value?.panes && value?.views && typeof value.panes === "object" && typeof value.views === "object") {
      const validSession = (session: SessionSummary) => session && typeof session.session_id === "string"
        && session.target && ["local", "ssh"].includes(session.target.kind)
        && Number.isInteger(session.terminal_size?.columns) && session.terminal_size.columns >= 2
        && session.terminal_size.columns <= 65535 && Number.isInteger(session.terminal_size.rows)
        && session.terminal_size.rows >= 1 && session.terminal_size.rows <= 65535;
      return {
        panes: Object.fromEntries(Object.entries(value.panes as Cache["panes"]).filter(([, entry]) =>
          entry && validSession(entry.session) && typeof entry.payload === "string" && Number.isFinite(entry.saved_at))),
        views: Object.fromEntries(Object.entries(value.views as Cache["views"]).filter(([, entry]) =>
          entry && validSession(entry.session) && entry.view?.session_id === entry.session.session_id
          && Array.isArray(entry.view.panes) && Array.isArray(entry.view.terminals) && Number.isFinite(entry.saved_at))),
      };
    }
  } catch { /* A missing or invalid cache must not prevent a live connection. */ }
  return { panes: {}, views: {} };
}

function save(cache: Cache) {
  const cutoff = Date.now() - MAX_AGE;
  for (const entries of [cache.panes, cache.views]) {
    for (const [key, entry] of Object.entries(entries)) {
      if (entry.saved_at < cutoff) delete entries[key];
    }
  }
  const entries = Object.entries(cache.panes).sort((a, b) => a[1].saved_at - b[1].saved_at);
  let serialized = JSON.stringify(cache);
  while (serialized.length * 2 > MAX_BYTES && entries.length) {
    delete cache.panes[entries.shift()![0]];
    serialized = JSON.stringify(cache);
  }
  const views = Object.entries(cache.views).sort((a, b) => a[1].saved_at - b[1].saved_at);
  while (serialized.length * 2 > MAX_BYTES && views.length) {
    delete cache.views[views.shift()![0]];
    serialized = JSON.stringify(cache);
  }
  try { localStorage.setItem(STORAGE_KEY, serialized); }
  catch { /* Storage may be unavailable or full; the live renderer remains usable. */ }
}

function paneKey(session: SessionSummary): string {
  return JSON.stringify([sessionKey(session), session.terminal_id]);
}

export function saveTerminalSnapshot(snapshot: CachedTerminalSnapshot) {
  if (!snapshot.session.terminal_id) return;
  const cache = read();
  cache.panes[paneKey(snapshot.session)] = snapshot;
  save(cache);
}

export function loadTerminalSnapshot(session: SessionSummary): CachedTerminalSnapshot | null {
  const cache = read();
  const snapshot = session.terminal_id ? cache.panes[paneKey(session)]
    : Object.values(cache.panes).filter((entry) => sessionKey(entry.session) === sessionKey(session))
      .sort((a, b) => b.saved_at - a.saved_at)[0];
  return snapshot && snapshot.saved_at >= Date.now() - MAX_AGE ? snapshot : null;
}

export function saveSessionView(session: SessionSummary, view: SessionView) {
  const cache = read();
  cache.views[sessionKey(session)] = { session, view, saved_at: Date.now() };
  save(cache);
}

export function loadSessionView(key: string): CachedSessionView | null {
  const entry = read().views[key];
  return entry && entry.saved_at >= Date.now() - MAX_AGE ? entry : null;
}
