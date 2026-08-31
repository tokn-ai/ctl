import { describe, expect, it } from "vitest";
import {
  DAEMON_RESTART_UNSUPPORTED,
  restartFailurePreservesLocalState,
} from "./restartFailurePolicy";

describe("restart failure policy", () => {
  it("preserves the active local view only for a safe preflight refusal", () => {
    expect(
      restartFailurePreservesLocalState(DAEMON_RESTART_UNSUPPORTED),
    ).toBe(true);
  });

  it("treats unknown and post-start failures as potentially destructive", () => {
    expect(restartFailurePreservesLocalState("daemon_restart_failed")).toBe(
      false,
    );
    expect(restartFailurePreservesLocalState(null)).toBe(false);
  });
});
