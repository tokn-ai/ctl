import { describe, expect, it } from "vitest";
import type { SessionSummary, TerminalSize } from "../../lib/types";
import { sessionKey } from "../targets/targets";
import {
  SessionListRefreshGuard,
  mergeTargetSessionLists,
  prependSession,
  removeSession,
  replaceSessionList,
  syncSessionTerminalSize,
  syncSessionObservation,
} from "./sessionListState";
import { hasKnownTerminalSize, mergeSessionObservation } from "./sessionObservation";

function terminalSize(columns: number, rows: number): TerminalSize {
  return {
    columns,
    rows,
    pixel_width: null,
    pixel_height: null,
  };
}

function session(
  sessionId: string,
  overrides: Partial<SessionSummary> = {},
): SessionSummary {
  return {
    target: { kind: "local" },
    session_id: sessionId,
    name: sessionId,
    status: "running",
    terminal_size: terminalSize(80, 24),
    next_sequence: "10",
    ...overrides,
  };
}

describe("session list state", () => {
  it("synchronizes size and observed time without changing runtime status or other rows", () => {
    const original = session("observed", { status: "unknown", last_seen_at_ms: 100 });
    const other = session("other");
    const incoming = { ...original, status: "running" as const, name: "different", terminal_size: terminalSize(120, 42), last_seen_at_ms: 200 };
    const result = syncSessionObservation([original, other], sessionKey(original), incoming);
    expect(result[0]).toMatchObject({ status: "unknown", name: "observed", terminal_size: terminalSize(120, 42), last_seen_at_ms: 200 });
    expect(result[1]).toBe(other);
    expect(syncSessionObservation(result, sessionKey(original), incoming)).toBe(result);
    expect(syncSessionObservation(result, sessionKey(original), { ...incoming, last_seen_at_ms: 50 })).toBe(result);
    expect(syncSessionObservation(result, sessionKey(other), incoming)).toBe(result);
  });

  it("preserves fresh observations across list replacements and older inspections", () => {
    const known = session("known", { last_seen_at_ms: 200, terminal_size: terminalSize(120, 42) });
    const stale = session("known", { last_seen_at_ms: 100, status: "exited" });
    const expected = { ...stale, last_seen_at_ms: 200, terminal_size: known.terminal_size };
    expect(replaceSessionList([stale], [known])[0]).toMatchObject(expected);
    expect(prependSession([known], stale)[0]).toMatchObject(expected);
    expect(mergeTargetSessionLists([known], [known.target], new Map([["local", [stale]]]))[0]).toMatchObject(expected);
    expect(mergeSessionObservation(known, { ...stale, last_seen_at_ms: undefined })).toMatchObject({ last_seen_at_ms: 200, terminal_size: known.terminal_size });
  });

  it("allows geometry changes at equal observation time and never promotes a restored placeholder", () => {
    const unknown = session("known", { terminal_size_known: false, last_seen_at_ms: 200 });
    const timeOnly = { ...unknown, last_seen_at_ms: 300 };
    const updated = syncSessionObservation([unknown], sessionKey(unknown), timeOnly);
    expect(hasKnownTerminalSize(updated[0])).toBe(false);
    const observed = { ...timeOnly, terminal_size_known: true, terminal_size: terminalSize(120, 42) };
    const known = syncSessionObservation(updated, sessionKey(unknown), observed);
    expect(known[0].terminal_size).toEqual(observed.terminal_size);
    expect(hasKnownTerminalSize(known[0])).toBe(true);
    expect(syncSessionObservation(known, sessionKey(unknown), { ...timeOnly, last_seen_at_ms: 400 })[0]).toMatchObject({ terminal_size: observed.terminal_size, last_seen_at_ms: 400 });
  });

  it("does not create evidence from invalid or missing timestamps on legacy placeholders", () => {
    const unknown = session("known", { terminal_size_known: false });
    const current = [unknown];
    for (const last_seen_at_ms of [undefined, null, 0, -1, NaN, 8_640_000_000_000_001]) {
      expect(syncSessionObservation(current, sessionKey(unknown), { ...unknown, last_seen_at_ms })).toBe(current);
    }
  });

  it("fills unknown geometry from an older real observation without regressing known time", () => {
    const timeOnly = session("known", { terminal_size_known: false, last_seen_at_ms: 200 });
    const observed = session("known", { terminal_size_known: true, terminal_size: terminalSize(120, 40), last_seen_at_ms: 100 });
    const current = syncSessionObservation([timeOnly], sessionKey(timeOnly), observed);
    expect(current[0]).toMatchObject({ terminal_size: observed.terminal_size, terminal_size_known: true, last_seen_at_ms: 200 });
    expect(syncSessionObservation(current, sessionKey(timeOnly), { ...timeOnly, last_seen_at_ms: 150 })).toBe(current);
  });

  it("accepts only the latest overlapping refresh", () => {
    const guard = new SessionListRefreshGuard();
    const first = guard.begin();
    const second = guard.begin();

    expect(guard.canApply(first)).toBe(false);
    expect(guard.isLatest(first)).toBe(false);
    expect(guard.canApply(second)).toBe(true);
    expect(guard.isLatest(second)).toBe(true);
  });

  it("rejects a refresh captured before a local list mutation", () => {
    const guard = new SessionListRefreshGuard();
    const refresh = guard.begin();

    guard.recordMutation();

    expect(guard.canApply(refresh)).toBe(false);
    expect(guard.isLatest(refresh)).toBe(true);
  });

  it("replaces the complete list with the refreshed sessions", () => {
    const current = [session("old")];
    const refreshed = [session("first"), session("second")];

    const result = replaceSessionList(refreshed);

    expect(result).toEqual(refreshed);
    expect(result).not.toBe(refreshed);
    expect(result).not.toContain(current[0]);
  });

  it("prepends a created session and removes an older copy", () => {
    const stale = session("created", { name: "stale" });
    const created = session("created", { name: "fresh" });
    const other = session("other");

    const result = prependSession([other, stale], created);

    expect(result).toEqual([created, other]);
    expect(result.filter((item) => item.session_id === "created")).toHaveLength(1);
  });

  it("keeps equal daemon session ids distinct across targets", () => {
    const local = session("same");
    const remote = session("same", {
      target: { kind: "ssh", destination: "ctmux-docker" },
    });

    expect(prependSession([local], remote)).toEqual([remote, local]);
  });

  it("replaces successful hosts while retaining failed-host rows", () => {
    const localOld = session("local-old");
    const localNew = session("local-new");
    const remote = session("remote", {
      target: { kind: "ssh", destination: "ctmux-docker" },
    });
    const targets = [localOld.target, remote.target];

    expect(
      mergeTargetSessionLists(
        [localOld, remote],
        targets,
        new Map([["local", [localNew]]]),
      ),
    ).toEqual([localNew, remote]);
  });

  it("synchronizes only terminal_size on an existing session", () => {
    const original = session("active", {
      name: "shell",
      status: "exited",
      next_sequence: "42",
    });
    const other = session("other");
    const resized = terminalSize(107, 24);

    const result = syncSessionTerminalSize(
      [original, other],
      sessionKey(original),
      resized,
    );

    expect(result).toEqual([
      {
        ...original,
        terminal_size: resized,
      },
      other,
    ]);
    expect(result[0]).toMatchObject({
      name: "shell",
      status: "exited",
      next_sequence: "42",
    });
    expect(result[1]).toBe(other);
  });

  it("does not upsert a session that is no longer in the list", () => {
    const current = [session("remaining")];

    const result = syncSessionTerminalSize(
      current,
      sessionKey(session("removed")),
      terminalSize(107, 24),
    );

    expect(result).toBe(current);
    expect(result).toEqual(current);
  });

  it("removes a session by session_id", () => {
    const first = session("first");
    const removed = session("removed");
    const second = session("second");

    expect(removeSession([first, removed, second], sessionKey(removed))).toEqual([
      first,
      second,
    ]);
  });
});
