import { describe, expect, it } from "vitest";
import type { AttachmentStatisticsEvent, SessionSummary } from "../../lib/types";
import { TerminalStatisticsStore, type StatisticsSource } from "./TerminalStatisticsStore";

const session: SessionSummary = {
  target: { kind: "local" }, session_id: "session", terminal_id: "primary", name: "shell", status: "running", next_sequence: "0",
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
};

function source(attachment_id: string, overrides: Partial<StatisticsSource> = {}): StatisticsSource {
  return { session, phase: "attached", attachment_id, history_bytes: () => 20, ...overrides };
}

function sample(attachment_id: string, overrides: Partial<AttachmentStatisticsEvent> = {}): AttachmentStatisticsEvent {
  return { event_type: "attachment_statistics", attachment_id, uploaded_bytes: "100", downloaded_bytes: "200", round_trip_ms: 10,
    event_lag_ms: 2, pending_events: 1, history_session_id: "session", history_retained_bytes: "1000", ...overrides };
}

describe("page terminal statistics", () => {
  it("counts real received events and aggregates all streams using elapsed monotonic time", () => {
    let now = 100;
    const store = new TerminalStatisticsStore(() => now);
    const primary = Symbol(), hidden = Symbol();
    store.update(primary, source("primary"));
    store.update(hidden, source("hidden", { session: { ...session, session_id: "background" } }));
    store.received({ event_type: "output", attachment_id: "primary", event_id: "1", sequence_start: "0", sequence_end: "1", data_base64: "eA==" });
    store.received({ event_type: "session_observed", attachment_id: "primary", last_seen_at_ms: 1 });
    store.received(sample("primary"));
    store.sample(primary, sample("primary"));
    store.sample(hidden, sample("hidden", { uploaded_bytes: "50", downloaded_bytes: "300" }));
    now += 1500;
    store.tick();
    expect(store.snapshot(session)).toMatchObject({ events_received: 1n, uploaded_bytes: 150n, downloaded_bytes: 500n,
      upload_bytes_per_second: 100, download_bytes_per_second: 500 / 1.5 });
    now += 500;
    store.tick();
    expect(store.snapshot(session).download_bytes_per_second).toBe(0);
    store.remove(primary);
    store.remove(hidden);
    now += 500;
    store.tick();
    expect(store.snapshot(session)).toMatchObject({ events_received: 1n, downloaded_bytes: 500n, upload_bytes_per_second: 0, round_trip_ms: null });
  });

  it("preserves exact cumulative totals across reconnect and lower counter resets", () => {
    const store = new TerminalStatisticsStore();
    const id = Symbol();
    store.update(id, source("first"));
    const huge = 9007199254740993n;
    store.sample(id, sample("first", { uploaded_bytes: huge.toString() }));
    store.sample(id, sample("first", { uploaded_bytes: (huge + 3n).toString() }));
    store.update(id, source("second"));
    store.sample(id, sample("second", { uploaded_bytes: "5" }));
    store.sample(id, sample("second", { uploaded_bytes: "1" }));
    expect(store.snapshot(session).uploaded_bytes).toBe(huge + 9n);
  });

  it("sums unique rendered panes but uses the latest live session-wide gauge, including decreases", () => {
    const store = new TerminalStatisticsStore();
    const primary = Symbol(), pane = Symbol(), ended = Symbol(), duplicate = Symbol();
    store.update(primary, source("primary"));
    store.update(pane, source("pane", { session: { ...session, terminal_id: "pane" }, history_bytes: () => 30 }));
    store.update(ended, source("ended", { phase: "ended", session: { ...session, terminal_id: "old" }, history_bytes: () => 0 }));
    store.sample(primary, sample("primary"));
    store.sample(ended, sample("ended", { history_retained_bytes: "9000", round_trip_ms: 500 }));
    store.sample(pane, sample("pane", { history_retained_bytes: "100", round_trip_ms: 20, event_lag_ms: 7, pending_events: 2 }));
    store.tick();
    expect(store.snapshot(session)).toMatchObject({ history_downloaded_bytes: 50n, history_retained_bytes: 100n,
      round_trip_ms: 20, event_lag_ms: 7, pending_events: 3 });
    store.update(duplicate, source("replacement", { history_bytes: () => 0 }));
    store.sample(duplicate, sample("replacement", { history_retained_bytes: "30" }));
    store.tick();
    expect(store.snapshot(session)).toMatchObject({ history_downloaded_bytes: 30n, history_retained_bytes: 30n });
  });

  it("does not bind old gauges after a pane moves or an unsupported daemon reconnects", () => {
    const store = new TerminalStatisticsStore();
    const id = Symbol();
    store.update(id, source("old"));
    store.sample(id, sample("old"));
    store.tick();
    store.update(id, source("old", { phase: "disconnected" }));
    expect(store.snapshot(session)).toMatchObject({ history_retained_bytes: 1000n, history_stale: true, round_trip_ms: null });
    store.update(id, source("new"));
    expect(store.snapshot(session)).toMatchObject({ history_retained_bytes: null, history_stale: false, round_trip_ms: null });
    const moved = { ...session, session_id: "other" };
    store.update(id, source("new", { session: moved }));
    store.sample(id, sample("new"));
    expect(store.snapshot(moved).history_retained_bytes).toBeNull();
    expect(store.snapshot(session).history_downloaded_bytes).toBeNull();
  });

  it("keeps identically named sessions on different targets separate", () => {
    const store = new TerminalStatisticsStore();
    const remote = { ...session, target: { kind: "ssh" as const, destination: "example.com", use_ssh_config: true } };
    store.update(Symbol(), source("local"));
    store.update(Symbol(), source("remote", { session: remote, history_bytes: () => 70 }));
    store.tick();
    expect(store.snapshot(session).history_downloaded_bytes).toBe(20n);
    expect(store.snapshot(remote).history_downloaded_bytes).toBe(70n);
  });
});
