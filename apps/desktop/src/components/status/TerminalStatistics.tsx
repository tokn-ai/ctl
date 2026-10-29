import { useEffect, useId, useRef, useState } from "react";
import { Icon } from "../ui/Icon";
import { createPortal } from "react-dom";
import type { SessionSummary } from "../../lib/types";
import { useTerminalStatistics } from "../../features/statistics/TerminalStatisticsContext";
import { formatStatisticsBytes, formatStatisticsDuration, formatStatisticsHistory, formatStatisticsRate } from "./statisticsFormat";

export function TerminalStatistics({ session, inert = false }: { session: SessionSummary | null; inert?: boolean }) {
  const statistics = useTerminalStatistics(session);
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLElement>(null);
  const panel_id = useId();
  useEffect(() => {
    if (!open) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
        trigger.current?.focus();
      }
    };
    const outside = (event: PointerEvent) => {
      if (event.target instanceof Node && !panel.current?.contains(event.target) && !trigger.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("keydown", close);
    document.addEventListener("pointerdown", outside);
    return () => {
      document.removeEventListener("keydown", close);
      document.removeEventListener("pointerdown", outside);
    };
  }, [open]);
  useEffect(() => { if (inert) setOpen(false); }, [inert]);
  if (!statistics) return null;
  const history = formatStatisticsHistory(statistics.history_downloaded_bytes, statistics.history_retained_bytes);
  const history_title = "Selected session: downloaded / full retained history text across its panes, in UTF-8 bytes including line endings. Equal sizes appear once. Live screen cells and styles are excluded.";
  const history_state = statistics.history_stale ? " Last observed before disconnect; the server may have changed." : "";
  return <div className="terminal-statistics">
    <button type="button" ref={trigger} aria-label="Terminal statistics" aria-expanded={open && !inert} aria-controls={panel_id}
      disabled={inert} onClick={() => setOpen((current) => !current)} title="Terminal statistics for this page and selected session">
      <Icon name="activity" size={14} />
    </button>
    {open && !inert && createPortal(<section ref={panel} id={panel_id} className="terminal-statistics-details" aria-label="Terminal statistics details">
      <strong>Page traffic</strong>
      <dl>
        <dt>Events received</dt><dd>{statistics.events_received.toLocaleString()}</dd>
        <dt>Upload</dt><dd>{formatStatisticsRate(statistics.upload_bytes_per_second)}</dd>
        <dt>Download</dt><dd>{formatStatisticsRate(statistics.download_bytes_per_second)}</dd>
        <dt>Bytes uploaded</dt><dd>{formatStatisticsBytes(statistics.uploaded_bytes)}</dd>
        <dt>Bytes downloaded</dt><dd>{formatStatisticsBytes(statistics.downloaded_bytes)}</dd>
      </dl>
      <p>All tabs and panes opened in this page. Traffic includes protocol framing, excluding SSH encryption and the initial handshake. Counters reset when this page reloads.</p>
      <strong>Selected session</strong>
      <dl>
        <dt>History downloaded / retained</dt><dd>{session ? history : "—"}</dd>
        <dt>Heartbeat round-trip</dt><dd>{formatStatisticsDuration(statistics.round_trip_ms)}</dd>
        <dt>Event lag</dt><dd>{formatStatisticsDuration(statistics.event_lag_ms)}</dd>
        <dt>Pending events</dt><dd>{statistics.pending_events.toLocaleString()}</dd>
      </dl>
      <p>{history_title}{history_state} {statistics.history_retained_bytes === null ? "Full retained history is unavailable until a supported daemon reports it." : ""}</p>
      <p>Timing shows the maximum across this session’s live pane streams; pending events are summed. Lag shows the oldest pending event’s local waiting time, or the last completed event’s delay. It includes client queues and renderer acknowledgements, and excludes unread output still waiting upstream. Unavailable measurements appear as —.</p>
    </section>, document.body)}
  </div>;
}
