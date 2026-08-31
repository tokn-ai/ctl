import { describe, expect, it } from "vitest";
import type { ShellStateSummary } from "../../lib/types";
import {
  forgetTabShellState,
  rememberTabShellState,
  retainTabShellStates,
} from "./shellStateCache";

function shellState(revision: string, cwd: string): ShellStateSummary {
  return {
    shell_type: "zsh",
    cwd,
    running_command: null,
    prompt_phase: "at_prompt",
    tui_hint: "inline",
    revision,
    observed_sequence: revision,
  };
}

describe("tab shell-state cache", () => {
  it("keeps the newest snapshot for an inactive local tab", () => {
    const first = shellState("2", "/first");
    const second = shellState("10", "/second");
    const cache = rememberTabShellState(new Map(), "session-1", first);

    expect(rememberTabShellState(cache, "session-1", first)).toBe(cache);
    expect(rememberTabShellState(cache, "session-1", shellState("1", "/old"))).toBe(
      cache,
    );
    expect(rememberTabShellState(cache, "session-1", second).get("session-1")).toBe(
      second,
    );
  });

  it("drops snapshots only after their sessions leave this window", () => {
    const cache = rememberTabShellState(
      rememberTabShellState(new Map(), "session-1", shellState("1", "/one")),
      "session-2",
      shellState("1", "/two"),
    );

    const retained = retainTabShellStates(cache, new Set(["session-2"]));
    expect([...retained.keys()]).toEqual(["session-2"]);
    expect(forgetTabShellState(retained, "session-2").size).toBe(0);
  });

  it("replaces an equally recent snapshot when attachment visibility changes", () => {
    const visible = {
      ...shellState("4", "/workspace"),
      prompt_phase: "running" as const,
      running_command: "cargo test",
    };
    const redacted = {
      ...visible,
      running_command: null,
    };
    const cache = rememberTabShellState(new Map(), "session-1", visible);

    expect(
      rememberTabShellState(cache, "session-1", redacted).get("session-1"),
    ).toBe(visible);
    expect(
      rememberTabShellState(cache, "session-1", redacted, {
        replaceEqualRevision: true,
      }).get("session-1"),
    ).toBe(redacted);
  });
});
