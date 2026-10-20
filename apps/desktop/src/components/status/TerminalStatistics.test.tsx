// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "../../lib/types";
import { TerminalStatisticsProvider } from "../../features/statistics/TerminalStatisticsContext";
import { TerminalStatisticsStore } from "../../features/statistics/TerminalStatisticsStore";
import { TerminalStatistics } from "./TerminalStatistics";

afterEach(cleanup);

const session: SessionSummary = { target: { kind: "local" }, session_id: "session", terminal_id: "pane", name: "shell", status: "running",
  next_sequence: "0", terminal_size: { columns: 80, rows: 24, pixel_width: null, pixel_height: null } };

describe("terminal statistics status", () => {
  it("keeps complete details accessible and updates history/rates without new terminal events", () => {
    let now = 0;
    const store = new TerminalStatisticsStore(() => now);
    const id = Symbol();
    const source = { session, phase: "attached" as const, attachment_id: "connection", history_bytes: () => 1024 };
    store.update(id, source);
    store.sample(id, { event_type: "attachment_statistics", attachment_id: "connection", uploaded_bytes: "100", downloaded_bytes: "200",
      round_trip_ms: 12, event_lag_ms: 30, pending_events: 1, history_session_id: "session", history_retained_bytes: "2048" });
    now = 500;
    store.tick();
    render(<TerminalStatisticsProvider store={store}><TerminalStatistics session={session} /></TerminalStatisticsProvider>);
    expect(screen.getByText("History 1 KiB / 2 KiB")).toBeTruthy();
    const summary = screen.getByLabelText("Terminal statistics");
    fireEvent.click(summary);
    expect(summary.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("region", { name: "Terminal statistics details" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "Terminal statistics details" }).parentElement).toBe(document.body);
    expect(screen.getByText("Bytes downloaded")).toBeTruthy();
    expect(screen.getByText(/unread output still waiting upstream/)).toBeTruthy();
    act(() => { now = 1000; store.tick(); });
    expect(screen.getAllByText("0 B/s").length).toBeGreaterThan(0);
    act(() => { store.update(id, { ...source, phase: "disconnected" }); store.tick(); });
    expect(screen.getByText(/Last observed before disconnect/)).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(summary.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(summary);
    fireEvent.pointerDown(document.body);
    expect(summary.getAttribute("aria-expanded")).toBe("false");
  });
});
