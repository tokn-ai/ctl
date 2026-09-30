import { useEffect, useState } from "react";
import { isObservationTime } from "./sessionObservation";

export { isObservationTime } from "./sessionObservation";

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

export function lastSeenAge(observed_at_ms: unknown, now_ms: number): string | null {
  if (!isObservationTime(observed_at_ms)) return null;
  const elapsed = Math.max(0, now_ms - observed_at_ms);
  if (elapsed < MINUTE_MS) return "just now";
  if (elapsed < HOUR_MS) return `${Math.floor(elapsed / MINUTE_MS)}m ago`;
  if (elapsed < DAY_MS) return `${Math.floor(elapsed / HOUR_MS)}h ago`;
  return `${Math.floor(elapsed / DAY_MS)}d ago`;
}

/** One clock for all visible session ages; attached-only lists need no timer. */
export function useLastSeenClock(enabled: boolean): number {
  const [now_ms, setNow] = useState(Date.now);
  useEffect(() => {
    if (!enabled) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), MINUTE_MS);
    return () => window.clearInterval(timer);
  }, [enabled]);
  return now_ms;
}
