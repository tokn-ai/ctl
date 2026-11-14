import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useId, useRef, useState } from "react";
import { Icon } from "../ui/Icon";
import { createPortal } from "react-dom";
import type { SessionSummary } from "../../lib/types";
import { useTerminalStatistics } from "../../features/statistics/TerminalStatisticsContext";
import { formatStatisticsBytes, formatStatisticsDuration, formatStatisticsHistory, formatStatisticsRate } from "./statisticsFormat";

export function TerminalStatistics({ session, inert = false }: { session: SessionSummary | null; inert?: boolean }) {
  const { t, locale } = useLocale();
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
  const history = formatStatisticsHistory(statistics.history_downloaded_bytes, statistics.history_retained_bytes, locale);
  const history_title = t("statistics.history_description");
  const history_state = statistics.history_stale ? t("statistics.history_stale") : "";
  return <div className="terminal-statistics">
    <button type="button" ref={trigger} aria-label={t("statistics.label")} aria-expanded={open && !inert} aria-controls={panel_id}
      disabled={inert} onClick={() => setOpen((current) => !current)} title={t("statistics.tooltip")}>
      <Icon name="activity" size={14} />
    </button>
    {open && !inert && createPortal(<section ref={panel} id={panel_id} className="terminal-statistics-details" aria-label={t("statistics.section")}>
      <strong>{t("statistics.page_traffic")}</strong>
      <dl>
        <dt>{t("statistics.events_received")}</dt><dd>{statistics.events_received.toLocaleString(locale)}</dd>
        <dt>{t("statistics.upload")}</dt><dd>{formatStatisticsRate(statistics.upload_bytes_per_second, locale)}</dd>
        <dt>{t("statistics.download")}</dt><dd>{formatStatisticsRate(statistics.download_bytes_per_second, locale)}</dd>
        <dt>{t("statistics.bytes_uploaded")}</dt><dd>{formatStatisticsBytes(statistics.uploaded_bytes, locale)}</dd>
        <dt>{t("statistics.bytes_downloaded")}</dt><dd>{formatStatisticsBytes(statistics.downloaded_bytes, locale)}</dd>
      </dl>
      <p>{t("statistics.traffic_description")}</p>
      <strong>{t("statistics.selected_session")}</strong>
      <dl>
        <dt>{t("statistics.history_sizes")}</dt><dd>{session ? history : "—"}</dd>
        <dt>{t("statistics.heartbeat")}</dt><dd>{formatStatisticsDuration(statistics.round_trip_ms, locale)}</dd>
        <dt>{t("statistics.event_lag")}</dt><dd>{formatStatisticsDuration(statistics.event_lag_ms, locale)}</dd>
        <dt>{t("statistics.pending_events")}</dt><dd>{statistics.pending_events.toLocaleString(locale)}</dd>
      </dl>
      <p>{history_title}{history_state} {statistics.history_retained_bytes === null ? t("statistics.history_unavailable") : ""}</p>
      <p>{t("statistics.timing_description")}</p>
    </section>, document.body)}
  </div>;
}
