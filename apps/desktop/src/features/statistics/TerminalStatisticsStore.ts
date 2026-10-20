import type { AttachmentEvent, AttachmentStatisticsEvent, ConnectionPhase, SessionSummary } from "../../lib/types";
import { targetKey } from "../targets/targets";

export interface StatisticsSource {
  session: SessionSummary | null;
  phase: ConnectionPhase;
  attachment_id: string | null;
  history_bytes?: () => number | null;
}

interface Stream {
  source: StatisticsSource;
  statistics: AttachmentStatisticsEvent | null;
  uploaded_bytes: bigint;
  downloaded_bytes: bigint;
  history_bytes: bigint | null;
  sample_order: number;
  source_order: number;
}

export interface TerminalStatisticsSnapshot {
  events_received: bigint;
  uploaded_bytes: bigint;
  downloaded_bytes: bigint;
  upload_bytes_per_second: number;
  download_bytes_per_second: number;
  history_downloaded_bytes: bigint | null;
  history_retained_bytes: bigint | null;
  history_stale: boolean;
  round_trip_ms: number | null;
  event_lag_ms: number | null;
  pending_events: number;
}

function bytes(value: string | null): bigint | null {
  return value !== null && /^\d+$/.test(value) ? BigInt(value) : null;
}

function maximum(values: (number | null | undefined)[]): number | null {
  const valid = values.filter((value): value is number => value !== null && value !== undefined && Number.isFinite(value) && value >= 0);
  return valid.length ? Math.max(...valid) : null;
}

export class TerminalStatisticsStore {
  private readonly streams = new Map<symbol, Stream>();
  private readonly listeners = new Set<() => void>();
  private events_received = 0n;
  private uploaded_bytes = 0n;
  private downloaded_bytes = 0n;
  private previous_upload = 0n;
  private previous_download = 0n;
  private previous_at: number;
  private upload_rate = 0;
  private download_rate = 0;
  private revision = 0;
  private sample_order = 0;
  private source_order = 0;

  constructor(private readonly now: () => number = () => performance.now()) {
    this.previous_at = now();
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  version = (): number => this.revision;

  update(id: symbol, source: StatisticsSource): void {
    const stream = this.streams.get(id);
    if (stream) {
      stream.source = source;
      stream.source_order = ++this.source_order;
    } else this.streams.set(id, { source, statistics: null, uploaded_bytes: 0n, downloaded_bytes: 0n, history_bytes: null, sample_order: 0, source_order: ++this.source_order });
  }

  remove(id: symbol): void {
    this.streams.delete(id);
  }

  /** Count delivery, before connection buffering or the renderer's ordered tail. */
  received(event: AttachmentEvent): void {
    if (event.event_type !== "attachment_statistics" && event.event_type !== "session_observed") this.events_received += 1n;
  }

  sample(id: symbol, event: AttachmentStatisticsEvent): void {
    const stream = this.streams.get(id);
    if (!stream) return;
    const uploaded = bytes(event.uploaded_bytes);
    const downloaded = bytes(event.downloaded_bytes);
    if (uploaded === null || downloaded === null) return;
    const same_connection = stream.statistics?.attachment_id === event.attachment_id;
    this.uploaded_bytes += same_connection && uploaded >= stream.uploaded_bytes ? uploaded - stream.uploaded_bytes : uploaded;
    this.downloaded_bytes += same_connection && downloaded >= stream.downloaded_bytes ? downloaded - stream.downloaded_bytes : downloaded;
    stream.uploaded_bytes = uploaded;
    stream.downloaded_bytes = downloaded;
    stream.statistics = event;
    stream.sample_order = ++this.sample_order;
  }

  tick(): void {
    const now = this.now();
    const elapsed = now - this.previous_at;
    const live = [...this.streams.values()].some(({ source }) => source.phase === "attached");
    if (elapsed > 0) {
      this.upload_rate = live ? Number(this.uploaded_bytes - this.previous_upload) * 1000 / elapsed : 0;
      this.download_rate = live ? Number(this.downloaded_bytes - this.previous_download) * 1000 / elapsed : 0;
      this.previous_at = now;
      this.previous_upload = this.uploaded_bytes;
      this.previous_download = this.downloaded_bytes;
    }
    for (const stream of this.streams.values()) {
      const value = stream.source.history_bytes?.();
      if (value !== undefined && value !== null && Number.isSafeInteger(value) && value >= 0) stream.history_bytes = BigInt(value);
    }
    this.revision += 1;
    for (const listener of this.listeners) listener();
  }

  snapshot(session: SessionSummary | null): TerminalStatisticsSnapshot {
    const selected = session ? [...this.streams.values()].filter(({ source }) =>
      source.session && targetKey(source.session.target) === targetKey(session.target) &&
      source.session.session_id === session.session_id) : [];
    const terminals = new Map<string, Stream>();
    for (const stream of selected) {
      const { source, history_bytes } = stream;
      if (history_bytes === null || !source.session) continue;
      const key = source.session.terminal_id ?? source.session.session_id;
      // A pane can temporarily have two streams during transfer/reconnect.
      const previous = terminals.get(key);
      if (!previous || (source.phase === "attached" && previous.source.phase !== "attached") ||
        (source.phase === previous.source.phase && stream.source_order > previous.source_order)) terminals.set(key, stream);
    }
    const live = selected.filter(({ source, statistics }) => source.phase === "attached" && statistics?.attachment_id === source.attachment_id);
    const gauges = selected.filter(({ source, statistics }) => statistics?.history_session_id === source.session?.session_id && bytes(statistics?.history_retained_bytes ?? null) !== null);
    const live_gauges = gauges.filter((stream) => live.includes(stream));
    const has_live_stream = selected.some(({ source }) => source.phase === "attached");
    const latest = (has_live_stream ? live_gauges : gauges).reduce<Stream | null>((latest, stream) =>
      latest === null || stream.sample_order > latest.sample_order ? stream : latest, null);
    const full = latest ? bytes(latest.statistics!.history_retained_bytes) : null;
    return {
      events_received: this.events_received,
      uploaded_bytes: this.uploaded_bytes,
      downloaded_bytes: this.downloaded_bytes,
      upload_bytes_per_second: this.upload_rate,
      download_bytes_per_second: this.download_rate,
      history_downloaded_bytes: terminals.size ? [...terminals.values()].reduce((sum, stream) => sum + stream.history_bytes!, 0n) : null,
      // The backend repeats a session-wide gauge on each pane stream.
      history_retained_bytes: full,
      history_stale: selected.length > 0 && !has_live_stream,
      round_trip_ms: maximum(live.map(({ statistics }) => statistics?.round_trip_ms)),
      event_lag_ms: maximum(live.map(({ statistics }) => statistics?.event_lag_ms)),
      pending_events: live.reduce((sum, { statistics }) => sum + (statistics?.pending_events ?? 0), 0),
    };
  }
}
