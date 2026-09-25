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
  const selected_lines = selected_archive?.terminals.find((terminal) => terminal.terminal_id === selected?.terminal_id)?.lines;
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
  return <dialog ref={dialog} className="archive-browser" aria-label="Archived sessions" onCancel={on_close}>
    <header><h2>Archived sessions</h2><button type="button" onClick={on_close}>Close</button></header>
    <p>Stored on this device for seven days. Retained text is read only.</p>
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
              <small title={new Date(archive.expires_at_ms).toLocaleString()}>Retained until {new Date(archive.expires_at_ms).toLocaleDateString()}</small>
              {archive.terminals.length === 1 && <small title={archive.terminals[0].reason}>{archive.terminals[0].reason}</small>}
            </span>
          </button>
          {archive.terminals.length > 1 && <div className="archive-card-panes" aria-label={`${archive.name} panes`}>
            {archive.terminals.map((terminal, index) => <button key={terminal.terminal_id} type="button"
              aria-pressed={active && selected?.terminal_id === terminal.terminal_id}
              onClick={() => setSelected({ archive_key, terminal_id: terminal.terminal_id })} title={terminal.reason}>
              Pane {index + 1}<span>{terminal.reason}</span>
            </button>)}
          </div>}
        </section>;
      })}</nav>
      <pre className="archive-terminal" aria-label="Archived terminal output">{selected_archive ? selected_lines?.join("\n") || "No cached output was available." : "Select an archived session."}</pre>
    </div>
  </dialog>;
}
