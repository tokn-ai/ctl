import { useEffect, useRef, useState } from "react";
import { sessionArchive } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import { targetKey, targetLabel } from "../../features/targets/targets";
import type { ConnectionTarget, SessionArchive } from "../../lib/types";
import { XtermRenderer } from "../../features/terminal/XtermRenderer";
import "./archiveBrowser.css";

export function ArchiveBrowser({ targets, on_close }: {
  targets: readonly ConnectionTarget[];
  on_close(): void;
}) {
  const [target_key, setTargetKey] = useState(() => targets[0] ? targetKey(targets[0]) : "");
  const target = targets.find((candidate) => targetKey(candidate) === target_key);
  const [archives, setArchives] = useState<SessionArchive[]>([]);
  const [selected, setSelected] = useState<{ session_id: string; terminal_id: string } | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [container, setContainer] = useState<HTMLDivElement | null>(null);
  const request_generation = useRef(0);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);

  useEffect(() => {
    let cancelled = false;
    ++request_generation.current;
    setArchives([]);
    setSelected(null);
    setError(null);
    if (!target) { setLoading(false); return; }
    setLoading(true);
    void sessionArchive(target, { kind: "list" }).then((response) => {
      if (cancelled) return;
      if (response.kind !== "list") throw new Error("Unexpected archive response");
      setArchives(response.archives);
    }).catch((failure) => { if (!cancelled) setError(errorMessage(failure)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [target_key, revision]);

  useEffect(() => {
    if (!container || !target || !selected) return;
    const generation = ++request_generation.current;
    const renderer = new XtermRenderer(container, () => {}, { columns: 80, rows: 24, pixel_width: null, pixel_height: null });
    setLoading(true);
    setError(null);
    void sessionArchive(target, { kind: "read", ...selected }).then(async (response) => {
      if (request_generation.current !== generation) return;
      if (response.kind !== "terminal") throw new Error("Unexpected archive response");
      const bytes = (value: string) => Uint8Array.from(atob(value), (character) => character.charCodeAt(0));
      await renderer.restoreCheckpoint(response.checkpoint.terminal_size, response.history.lines,
        bytes(response.checkpoint.payload_base64), bytes(response.checkpoint.input_prefix_base64), response.checkpoint.sequence);
    }).catch((failure) => { if (request_generation.current === generation) setError(errorMessage(failure)); })
      .finally(() => { if (request_generation.current === generation) setLoading(false); });
    return () => { ++request_generation.current; renderer.dispose(); };
  }, [container, target_key, selected]);

  return <dialog ref={dialog} className="archive-browser" aria-label="Archived sessions" onCancel={on_close}>
    <header>
      <h2>Archived sessions</h2>
      <button type="button" onClick={on_close}>Close</button>
    </header>
    <div className="archive-browser-controls">
      <label>Host <select value={target_key} onChange={(event) => setTargetKey(event.target.value)}>
        {targets.map((candidate) => <option key={targetKey(candidate)} value={targetKey(candidate)}>{targetLabel(candidate)}</option>)}
      </select></label>
      <button type="button" onClick={() => setRevision((value) => value + 1)} disabled={loading}>Refresh</button>
      <span>Read only. Select terminal text to copy.</span>
    </div>
    {loading && <p role="status">Loading archive…</p>}
    {error && <p role="alert">{error}</p>}
    {!loading && !error && !archives.length && <p>No retained archives on this host.</p>}
    <div className="archive-browser-content">
      <nav aria-label="Retained sessions">{archives.map((archive) => <section key={archive.session_id}>
        <strong>{archive.name}</strong>
        <small>Ended {new Date(archive.archived_at_ms).toLocaleString()} · Retained until {new Date(archive.expires_at_ms).toLocaleString()}</small>
        {archive.terminals.map((terminal, index) => <button key={terminal.terminal_id} type="button"
          aria-pressed={selected?.terminal_id === terminal.terminal_id && selected.session_id === archive.session_id}
          onClick={() => setSelected({ session_id: archive.session_id, terminal_id: terminal.terminal_id })}>
          Pane {index + 1}: {terminal.reason}{terminal.exit_code === null ? "" : ` (${terminal.exit_code})`}
        </button>)}
      </section>)}</nav>
      <div className="archive-terminal" ref={setContainer} aria-label="Archived terminal output" />
    </div>
  </dialog>;
}
