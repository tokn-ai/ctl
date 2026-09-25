import { describe, expect, it } from "vitest";
import type { ShellStateSummary } from "../../lib/types";
import {
  forgetShellState,
  mergeShellStateInspections,
  rememberShellState,
  retainShellStates,
} from "./shellStateCache";

function shellState(
  revision: string,
  cwd: string,
  runningCommand: string | null = null,
): ShellStateSummary {
  return {
    shell_type: "zsh",
    cwd,
    running_command: runningCommand,
    prompt_phase: runningCommand ? "running" : "at_prompt",
    tui_hint: "inline",
    revision,
    observed_sequence: revision,
  };
}

describe("shell-state cache", () => {
  it("keeps the newest snapshot for each session", () => {
    const first = shellState("2", "/first");
    const second = shellState("10", "/second");
    const cache = rememberShellState(new Map(), "session-1", first);

    expect(rememberShellState(cache, "session-1", first)).toBe(cache);
    expect(rememberShellState(cache, "session-1", shellState("1", "/old"))).toBe(
      cache,
    );
    expect(rememberShellState(cache, "session-1", second).get("session-1")).toBe(
      second,
    );
  });

  it("drops snapshots only after their sessions are no longer retained", () => {
    const cache = rememberShellState(
      rememberShellState(new Map(), "session-1", shellState("1", "/one")),
      "session-2",
      shellState("1", "/two"),
    );

    const retained = retainShellStates(cache, new Set(["session-2"]));
    expect([...retained.keys()]).toEqual(["session-2"]);
    expect(forgetShellState(retained, "session-2").size).toBe(0);
  });

  it("can replace an equal revision with a more authorized live snapshot", () => {
    const listed = shellState("2", "/work");
    const attached = shellState("2", "/work", "cargo test");
    const cache = rememberShellState(new Map(), "session-1", listed);

    expect(
      rememberShellState(cache, "session-1", attached, {
        replaceEqualRevision: true,
      }).get("session-1"),
    ).toBe(attached);
  });

  it("preserves cached metadata when best-effort inspection is absent", () => {
    const attached = shellState("2", "/work", "cargo test");
    const cache = new Map([["session-1", attached]]);

    const merged = mergeShellStateInspections(
      cache,
      new Map(),
      new Set(["session-1"]),
    );

    expect(merged.get("session-1")).toBe(attached);
  });

  it("retains authorized text at the same revision but accepts newer inspection", () => {
    const attached = shellState("2", "/work", "cargo test");
    const sameRevisionRedacted = shellState("2", "/work");
    const newerPrompt = shellState("3", "/next");
    const cache = new Map([["session-1", attached]]);
    const sessionIds = new Set(["session-1"]);

    const sameRevision = mergeShellStateInspections(
      cache,
      new Map([["session-1", sameRevisionRedacted]]),
      sessionIds,
    );
    expect(sameRevision.get("session-1")).toBe(attached);

    const newer = mergeShellStateInspections(
      sameRevision,
      new Map([["session-1", newerPrompt]]),
      sessionIds,
    );
    expect(newer.get("session-1")).toBe(newerPrompt);
  });
});
