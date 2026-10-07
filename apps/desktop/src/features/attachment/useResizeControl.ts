import { useSyncExternalStore } from "react";
import type { LeaseStatus, SessionSummary } from "../../lib/types";
import { sessionResizeControlStatus, sessionResizeWithWindow, subscribeLayoutOwners, type ResizeControlStatus } from "./componentActions";

/** The selected pane's lease can belong to a sibling open in this window. */
export function useResizeControl(session: SessionSummary | null, fallback: LeaseStatus | null): ResizeControlStatus {
  return useSyncExternalStore(subscribeLayoutOwners, () => {
    const status = sessionResizeControlStatus(session);
    if (status !== "unavailable" || !session || !fallback) return status;
    return fallback.owned_by_client ? "owned" : fallback.held ? "held_elsewhere" : "available";
  }, () => "unavailable");
}

export function useResizeWithWindow(session: SessionSummary | null, fallback: boolean): boolean {
  return useSyncExternalStore(subscribeLayoutOwners, () => sessionResizeWithWindow(session) ?? fallback, () => false);
}
