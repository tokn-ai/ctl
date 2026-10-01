// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { isObservationTime, lastSeenAge, useLastSeenClock } from "./lastSeen";

const now_ms = Date.parse("2026-10-01T12:00:00.000Z");

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("lastSeenAge", () => {
  it.each([
    [0, "just now"],
    [59_999, "just now"],
    [60_000, "1m ago"],
    [3_599_999, "59m ago"],
    [3_600_000, "1h ago"],
    [7_200_000, "2h ago"],
    [86_399_999, "23h ago"],
    [86_400_000, "1d ago"],
    [172_800_000, "2d ago"],
  ])("formats an observation %i milliseconds ago as %s", (elapsed, expected) => {
    expect(lastSeenAge(now_ms - elapsed, now_ms)).toBe(expected);
  });

  it("clamps future observations to just now after clock changes", () => {
    expect(lastSeenAge(now_ms + 3_600_000, now_ms)).toBe("just now");
  });

  it.each([undefined, null, 0, -1, NaN, Infinity, 1.5, "123", 8_640_000_000_000_001])(
    "does not invent an age for invalid metadata %s", (value) => {
      expect(isObservationTime(value)).toBe(false);
      expect(lastSeenAge(value, now_ms)).toBeNull();
    },
  );
});

describe("useLastSeenClock", () => {
  it("runs one minute clock only while needed and clears it on unmount", () => {
    vi.useFakeTimers();
    vi.setSystemTime(now_ms);
    const { result, rerender, unmount } = renderHook(({ enabled }) => useLastSeenClock(enabled), {
      initialProps: { enabled: false },
    });
    expect(vi.getTimerCount()).toBe(0);
    act(() => vi.advanceTimersByTime(60_000));
    rerender({ enabled: true });
    expect(result.current).toBe(now_ms + 60_000);
    expect(vi.getTimerCount()).toBe(1);
    act(() => vi.advanceTimersByTime(60_000));
    expect(result.current).toBe(now_ms + 120_000);
    rerender({ enabled: true });
    expect(vi.getTimerCount()).toBe(1);
    rerender({ enabled: false });
    expect(vi.getTimerCount()).toBe(0);
    rerender({ enabled: true });
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });
});
