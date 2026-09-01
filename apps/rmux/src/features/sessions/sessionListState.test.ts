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
} from "./sessionListState";

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
      target: { kind: "ssh", destination: "rmux-docker" },
    });

    expect(prependSession([local], remote)).toEqual([remote, local]);
  });

  it("replaces successful hosts while retaining failed-host rows", () => {
    const localOld = session("local-old");
    const localNew = session("local-new");
    const remote = session("remote", {
      target: { kind: "ssh", destination: "rmux-docker" },
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
