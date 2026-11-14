import { message, type DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { sessionArchive } from "../../lib/tauri";
import { localizedErrorText } from "../../lib/errors";
import { targetKey, targetLabel } from "../../features/targets/targets";
import type { ConnectionTarget, SessionArchive } from "../../lib/types";
import { Icon } from "../ui/Icon";
import { InlineFeedback } from "../feedback/InlineFeedback";
import "./archiveBrowser.css";

export function ArchiveBrowser({ targets, on_close }: {
  targets: readonly ConnectionTarget[];
  on_close(): void;
}) {
  const { t, locale, formatText } = useLocale();
  const [archives, setArchives] = useState<SessionArchive[]>([]);
  const [selected, setSelected] = useState<{ archive_key: string; terminal_id: string | null } | null>(null);
  const selected_archive = archives.find((archive) => JSON.stringify([archive.host_key, archive.session_id]) === selected?.archive_key);
  const selected_pane = selected_archive?.terminals.find((terminal) => terminal.terminal_id === selected?.terminal_id);
  const output_key = selected_archive && selected_pane ? JSON.stringify([selected_archive.host_key, selected_archive.session_id, selected_pane.terminal_id]) : null;
  const [output, setOutput] = useState<{ key: string; lines: string[]; next_offset: string | null; history_gap: boolean } | null>(null);
  const [reading, setReading] = useState(false);
  const read_generation = useRef(0);
  const selected_lines = output?.key === output_key ? output.lines : selected_pane?.lines;
  const history_gap = selected_pane?.history_gap || (output?.key === output_key && output.history_gap);
  const selected_text = selected_lines?.some((line) => line.trim().length > 0) ? selected_lines.join("\n") : null;
  const displayed_output = !selected_archive ? t("archive.select")
    : selected_text !== null ? selected_text
      : reading ? t("archive.loading_output")
        : output?.key === output_key && output.next_offset ? t("archive.empty_output")
          : t("archive.no_output");

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<DisplayText | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  useEffect(() => {
    let cancelled = false;
    void sessionArchive({ kind: "list" }).then((response) => {
      if (cancelled) return;
      if (response.kind !== "list") { setError(message("archive.unexpected")); return; }
      setArchives(response.archives);
    }).catch((failure) => { if (!cancelled) setError(localizedErrorText(failure)); })
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
        if (response.kind !== "output") { setError(message("archive.unexpected_output")); return; }
        setOutput({ key: output_key, lines: response.lines, next_offset: response.next_offset, history_gap: response.history_gap });
      })
      .catch((failure) => { if (generation === read_generation.current) setError(localizedErrorText(failure)); })
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
        if (response.kind !== "output") { setError(message("archive.unexpected_output")); return; }
        setOutput((previous) => previous?.key === output_key ? { ...previous, lines: [...previous.lines, ...response.lines], next_offset: response.next_offset, history_gap: previous.history_gap || response.history_gap } : previous);
      })
      .catch((failure) => { if (generation === read_generation.current) setError(localizedErrorText(failure)); })
      .finally(() => { if (generation === read_generation.current) setReading(false); });
  }
  return <dialog ref={dialog} className="archive-browser" aria-label={t("archive.title")} onCancel={on_close}>
    <header><h2>{t("archive.title")}</h2><button type="button" onClick={on_close}>{t("host_connect.close")}</button></header>
    <p>{t("archive.description")}</p>
    {loading && <p role="status">{t("archive.loading")}</p>}
    {error && <InlineFeedback>{formatText(error)}</InlineFeedback>}
    {!loading && !error && !archives.length && <p>{t("archive.empty")}</p>}
    {history_gap && <InlineFeedback severity="warning">{t("archive.partial")}</InlineFeedback>}
    <div className="archive-browser-content">
      <nav aria-label={t("archive.retained")}>{archives.map((archive) => {
        const target = targets.find((candidate) => targetKey(candidate) === archive.host_key);
        const archive_key = JSON.stringify([archive.host_key, archive.session_id]);
        const active = selected?.archive_key === archive_key;
        const host_label = target ? targetLabel(target, locale) : archive.host_key;
        return <section className={`session-row archive-card ${active ? "active" : ""}`} key={archive_key}>
          <button className="session-select archive-card-select" type="button" aria-pressed={active}
            onClick={() => setSelected({ archive_key, terminal_id: archive.terminals[0]?.terminal_id ?? null })}>
            <Icon name="terminal" class_name="session-icon" />
            <span className="session-copy">
              <strong title={archive.name}>{archive.name}</strong>
              <small title={host_label}>{host_label}</small>
              <small title={new Date(archive.archived_at_ms).toLocaleString(locale)}>{t("archive.archived_at", { date: new Date(archive.archived_at_ms).toLocaleDateString(locale) })}</small>
              {archive.terminals.length === 1 && <small title={archive.terminals[0].reason}>{archive.terminals[0].reason}</small>}
            </span>
          </button>
          <button type="button" onClick={() => {
            void sessionArchive({ kind: "delete", host_key: archive.host_key, session_id: archive.session_id })
              .then(() => { setError(null); setArchives((previous) => previous.filter((item) => item !== archive)); if (active) setSelected(null); })
              .catch((failure) => setError(localizedErrorText(failure)));
          }}>{t("archive.delete")}</button>
          {archive.terminals.length > 1 && <div className="archive-card-panes" aria-label={t("archive.panes", { name: archive.name })}>
            {archive.terminals.map((terminal, index) => <button key={terminal.terminal_id} type="button"
              aria-pressed={active && selected?.terminal_id === terminal.terminal_id}
              onClick={() => setSelected({ archive_key, terminal_id: terminal.terminal_id })} title={terminal.reason}>
              {t("archive.pane_n", { index: index + 1 })}<span>{terminal.reason}</span>
            </button>)}
          </div>}
        </section>;
      })}</nav>
      <pre className="archive-terminal" aria-label={t("archive.terminal_output")}>{displayed_output}</pre>
    </div>
    {output?.key === output_key && output?.next_offset && <button type="button" disabled={reading} onClick={readMore}>{reading ? t("archive.loading_more") : t("archive.more")}</button>}
  </dialog>;
}
