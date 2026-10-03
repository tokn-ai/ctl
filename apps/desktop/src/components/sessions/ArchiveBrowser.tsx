import { useEffect, useRef, useState } from "react";
import { sessionArchive } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import { targetKey, targetLabel } from "../../features/targets/targets";
import type { ConnectionTarget, SessionArchive } from "../../lib/types";
import { Icon } from "../ui/Icon";
import "./archiveBrowser.css";

export function ArchiveBrowser({ targets, on_close }: {
  targets: readonly ConnectionTarget[];
  on_close(): void;
}) {
  const [archives, setArchives] = useState<SessionArchive[]>([]);
  const [selected, setSelected] = useState<{ archive_key: string; terminal_id: string | null } | null>(null);
  const selected_archive = archives.find((archive) => JSON.stringify([archive.host_key, archive.session_id]) === selected?.archive_key);
  const selected_pane = selected_archive?.terminals.find((terminal) => terminal.terminal_id === selected?.terminal_id);
  const output_key = selected_archive && selected_pane ? JSON.stringify([selected_archive.host_key, selected_archive.session_id, selected_pane.terminal_id]) : null;
  const [output, setOutput] = useState<{ key: string; lines: string[]; next_offset: string | null } | null>(null);
  const [reading, setReading] = useState(false);
  const read_generation = useRef(0);
  const selected_lines = output?.key === output_key ? output.lines : selected_pane?.lines;
  const selected_text = selected_lines?.some((line) => line.trim().length > 0) ? selected_lines.join("\n") : null;
  const displayed_output = !selected_archive ? "Select an archived session."
    : selected_text !== null ? selected_text
      : reading ? "Loading retained output…"
        : output?.key === output_key && output.next_offset ? "No readable text in the loaded output."
          : "No readable retained output is available.";

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  useEffect(() => {
    let cancelled = false;
    void sessionArchive({ kind: "list" }).then((response) => {
      if (cancelled) return;
      if (response.kind !== "list") throw new Error("Unexpected archive response");
      setArchives(response.archives);
    }).catch((failure) => { if (!cancelled) setError(errorMessage(failure)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);
  useEffect(() => {
    const generation = ++read_generation.current;
    if (!selected_archive || !selected_pane || !output_key || selected_pane.lines.length) {
      setReading(false);
      return;
    }
    setReading(true);
    setError(null);
    void sessionArchive({ kind: "read", host_key: selected_archive.host_key, session_id: selected_archive.session_id, terminal_id: selected_pane.terminal_id, offset: "0" })
      .then((response) => {
        if (generation !== read_generation.current) return;
        if (response.kind !== "output") throw new Error("Unexpected archive output response");
        setOutput({ key: output_key, lines: response.lines, next_offset: response.next_offset });
      })
      .catch((failure) => { if (generation === read_generation.current) setError(errorMessage(failure)); })
      .finally(() => { if (generation === read_generation.current) setReading(false); });
    return () => { read_generation.current++; };
  }, [output_key, selected_archive, selected_pane]);

  function readMore() {
    if (!selected_archive || !selected_pane || !output || output.key !== output_key || !output.next_offset || reading) return;
    const generation = read_generation.current;
    setReading(true);
    setError(null);
    void sessionArchive({ kind: "read", host_key: selected_archive.host_key, session_id: selected_archive.session_id, terminal_id: selected_pane.terminal_id, offset: output.next_offset })
      .then((response) => {
        if (generation !== read_generation.current) return;
        if (response.kind !== "output") throw new Error("Unexpected archive output response");
        setOutput((previous) => previous?.key === output_key ? { ...previous, lines: [...previous.lines, ...response.lines], next_offset: response.next_offset } : previous);
      })
      .catch((failure) => { if (generation === read_generation.current) setError(errorMessage(failure)); })
      .finally(() => { if (generation === read_generation.current) setReading(false); });
  }
  return <dialog ref={dialog} className="archive-browser" aria-label="Archived sessions" onCancel={on_close}>
    <header><h2>Archived sessions</h2><button type="button" onClick={on_close}>Close</button></header>
    <p>Stored on this device until deleted. Retained text is read only.</p>
    {loading && <p role="status">Loading archive…</p>}
    {error && <p role="alert">{error}</p>}
    {!loading && !error && !archives.length && <p>No retained archives on this device.</p>}
    <div className="archive-browser-content">
      <nav aria-label="Retained sessions">{archives.map((archive) => {
        const target = targets.find((candidate) => targetKey(candidate) === archive.host_key);
        const archive_key = JSON.stringify([archive.host_key, archive.session_id]);
        const active = selected?.archive_key === archive_key;
        const host_label = target ? targetLabel(target) : archive.host_key;
        return <section className={`session-row archive-card ${active ? "active" : ""}`} key={archive_key}>
          <button className="session-select archive-card-select" type="button" aria-pressed={active}
            onClick={() => setSelected({ archive_key, terminal_id: archive.terminals[0]?.terminal_id ?? null })}>
            <Icon name="terminal" class_name="session-icon" />
            <span className="session-copy">
              <strong title={archive.name}>{archive.name}</strong>
              <small title={host_label}>{host_label}</small>
              <small title={new Date(archive.archived_at_ms).toLocaleString()}>Archived {new Date(archive.archived_at_ms).toLocaleDateString()}</small>
              {archive.terminals.length === 1 && <small title={archive.terminals[0].reason}>{archive.terminals[0].reason}</small>}
            </span>
          </button>
          <button type="button" onClick={() => {
            void sessionArchive({ kind: "delete", host_key: archive.host_key, session_id: archive.session_id })
              .then(() => { setError(null); setArchives((previous) => previous.filter((item) => item !== archive)); if (active) setSelected(null); })
              .catch((failure) => setError(errorMessage(failure)));
          }}>Delete archive</button>
          {archive.terminals.length > 1 && <div className="archive-card-panes" aria-label={`${archive.name} panes`}>
            {archive.terminals.map((terminal, index) => <button key={terminal.terminal_id} type="button"
              aria-pressed={active && selected?.terminal_id === terminal.terminal_id}
              onClick={() => setSelected({ archive_key, terminal_id: terminal.terminal_id })} title={terminal.reason}>
              Pane {index + 1}<span>{terminal.reason}</span>
            </button>)}
          </div>}
        </section>;
      })}</nav>
      <pre className="archive-terminal" aria-label="Archived terminal output">{displayed_output}</pre>
    </div>
    {output?.key === output_key && output?.next_offset && <button type="button" disabled={reading} onClick={readMore}>{reading ? "Loading…" : "Load more output"}</button>}
  </dialog>;
}
