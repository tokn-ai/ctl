import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import { acknowledgeComponentReconnect } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type { ComponentReconnectRequest, ComponentSessionsReset, SessionSummary } from "../../lib/types";
import { reconnectComponentAttachments, resetComponentAttachments } from "../attachment/componentActions";

export const COMPONENT_RECONNECT_EVENT = "about-reconnect-attachments";
export const COMPONENT_RESET_EVENT = "about-reset-sessions";

export function useComponentActionEvents(on_reset: (event: ComponentSessionsReset, affected: SessionSummary[]) => void) {
  const callback = useRef(on_reset);
  callback.current = on_reset;
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    const stops: (() => void)[] = [];
    const completed = new Set<string>();
    const listeners = [
      listen<ComponentSessionsReset>(COMPONENT_RESET_EVENT, ({ payload }) => {
        callback.current(payload, resetComponentAttachments(payload));
      }),
      listen<ComponentReconnectRequest>(COMPONENT_RECONNECT_EVENT, ({ payload }) => {
        if (completed.has(payload.action_id)) return;
        completed.add(payload.action_id);
        void reconnectComponentAttachments(payload.attachment_ids)
          .then((results) => acknowledgeComponentReconnect(payload.action_id, results))
          .catch((failure) => { if (!disposed) setError(`Could not acknowledge component reconnection: ${errorMessage(failure)}`); });
      }),
    ];
    for (const pending of listeners) void pending.then((stop) => { if (disposed) stop(); else stops.push(stop); }).catch((failure) => {
      if (!disposed) setError(`Could not observe component actions: ${errorMessage(failure)}`);
    });
    return () => {
      disposed = true;
      for (const stop of stops) stop();
    };
  }, []);
  return error;
}
