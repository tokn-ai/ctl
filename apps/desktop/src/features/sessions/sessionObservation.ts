import type { SessionSummary, TerminalSize } from "../../lib/types";
import { sessionKey } from "../targets/targets";

/** Only native observations establish evidence; restore/retry never stamp now. */
export function observedAt(session: Pick<SessionSummary, "last_seen_at_ms">): number | null {
  const value = session.last_seen_at_ms;
  return isObservationTime(value) ? value : null;
}

export function isObservationTime(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 && value <= 8_640_000_000_000_000;
}

export function knownTerminalSize(value: TerminalSize | null | undefined): value is TerminalSize {
  const dimension = (value: number) => Number.isInteger(value) && value > 0 && value <= 65535;
  const pixels = (value: number | null) => value === null || Number.isInteger(value) && value >= 0 && value <= 65535;
  return Boolean(value && dimension(value.columns) && dimension(value.rows) &&
    pixels(value.pixel_width) && pixels(value.pixel_height));
}

export function sameTerminalSize(left: TerminalSize, right: TerminalSize): boolean {
  return left.columns === right.columns && left.rows === right.rows &&
    left.pixel_width === right.pixel_width && left.pixel_height === right.pixel_height;
}

export function hasKnownTerminalSize(session: SessionSummary): boolean {
  return knownTerminalSize(session.terminal_size) && (session.terminal_size_known === true ||
    session.terminal_size_known !== false && observedAt(session) !== null);
}

/** Preserve newer evidence across list/inspection replacements, without
 * treating remembered metadata as proof of the incoming runtime status. */
export function mergeSessionObservation(current: SessionSummary | undefined, incoming: SessionSummary): SessionSummary {
  if (!current || sessionKey(current) !== sessionKey(incoming)) return incoming;
  const previous = observedAt(current);
  const observed = observedAt(incoming);
  const stale = previous !== null && (observed === null || observed < previous);
  const preserve_size = hasKnownTerminalSize(current) && (stale || incoming.terminal_size_known === false);
  if (!stale && !preserve_size) return incoming;
  return {
    ...incoming,
    ...(preserve_size ? { terminal_size: current.terminal_size, terminal_size_known: current.terminal_size_known } : {}),
    ...(stale ? { last_seen_at_ms: previous } : {}),
  };
}
