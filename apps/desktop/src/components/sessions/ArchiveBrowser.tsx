import { useEffect, useRef, useState } from "react";
import { sessionArchive } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import { targetKey, targetLabel } from "../../features/targets/targets";
import type { ConnectionTarget, SessionArchive } from "../../lib/types";
import "./archiveBrowser.css";

export function ArchiveBrowser({ targets, on_close }: {
  targets: readonly ConnectionTarget[];
  on_close(): void;
}) {
  const [archives, setArchives] = useState<SessionArchive[]>([]);
  const [selected, setSelected] = useState<string[] | null>(null);
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
        return <section key={`${archive.host_key}:${archive.session_id}`}>
          <strong>{archive.name}</strong>
          <small>{target ? targetLabel(target) : archive.host_key}</small>
          <small>Ended {new Date(archive.archived_at_ms).toLocaleString()} · Retained until {new Date(archive.expires_at_ms).toLocaleString()}</small>
          {archive.terminals.map((terminal, index) => <button key={terminal.terminal_id} type="button"
            aria-pressed={selected === terminal.lines} onClick={() => setSelected(terminal.lines)}>
            Pane {index + 1}: {terminal.reason}
          </button>)}
        </section>;
      })}</nav>
      <pre className="archive-terminal" aria-label="Archived terminal output">{selected ? selected.join("\n") || "No cached output was available." : "Select an archived pane."}</pre>
    </div>
  </dialog>;
}
