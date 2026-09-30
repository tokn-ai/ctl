import { useEffect, useMemo, useRef } from "react";
import type { AttachmentActions } from "../attachment/useAttachment";
import { useNotificationEnvironment } from "./NotificationContext";

/** Report from the attachment owner so hidden panes and teardown share one lifecycle. */
export function useAttachmentNotifications(attachment: Pick<AttachmentActions, "state" | "reconnect" | "connection_attempt">) {
  const registry = useNotificationEnvironment()?.attachments;
  const lifetime = useMemo(() => ({ owner: registry?.createOwner(), mounted: false }), [registry]);
  const latest = useRef(attachment);
  latest.current = attachment;
  useEffect(() => {
    lifetime.mounted = true;
    return () => {
      lifetime.mounted = false;
      // React StrictMode replays effects without starting a new connection.
      queueMicrotask(() => {
        if (!lifetime.mounted && lifetime.owner) registry?.remove(lifetime.owner);
      });
    };
  }, [registry, lifetime]);
  useEffect(() => {
    if (lifetime.owner) registry?.report(lifetime.owner, attachment.state, () => latest.current.reconnect(), attachment.connection_attempt);
  });
}
