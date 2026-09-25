// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import type { SessionSummary } from "../../lib/types";
import { loadTerminalSnapshot, saveTerminalSnapshot } from "./offlineCache";
const session: SessionSummary = {
  target: { kind: "local" }, session_id: "root", terminal_id: "a", name: "Root",
  status: "running", next_sequence: "0",
  terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null },
};
afterEach(() => { localStorage.clear(); vi.useRealTimers(); });

it("persists distinct PTYs across module reloads and expires old snapshots", async () => {
  vi.useFakeTimers();
  const second = { ...session, terminal_id: "b" };
  saveTerminalSnapshot({ session, payload: "left", saved_at: Date.now() });
  saveTerminalSnapshot({ session: second, payload: "right", saved_at: Date.now() });
  vi.resetModules();
  const reloaded = await import("./offlineCache");
  expect(reloaded.loadTerminalSnapshot(session)?.payload).toBe("left");
  expect(reloaded.loadTerminalSnapshot(second)?.payload).toBe("right");
  vi.advanceTimersByTime(8 * 24 * 60 * 60 * 1000);
  expect(reloaded.loadTerminalSnapshot(session)).toBeNull();
});

it("bounds persisted storage by evicting the oldest pane", () => {
  saveTerminalSnapshot({ session, payload: "a".repeat(600_000), saved_at: Date.now() - 1000 });
  const second = { ...session, terminal_id: "b" };
  saveTerminalSnapshot({ session: second, payload: "b".repeat(600_000), saved_at: Date.now() });
  expect(loadTerminalSnapshot(session)).toBeNull();
  expect(loadTerminalSnapshot(second)?.payload).toHaveLength(600_000);
  expect(localStorage.getItem("rmux.offline_views.v1")!.length * 2).toBeLessThanOrEqual(2_000_000);
});

it("ignores malformed persisted data", () => {
  localStorage.setItem("rmux.offline_views.v1", '{"panes":{"bad":null},"views":{}}');
  expect(loadTerminalSnapshot(session)).toBeNull();
});
