import { createContext, useContext, useEffect, useMemo, type ReactNode } from "react";
import type { NotificationInput } from "../../lib/types";
import type { NotificationStore } from "./NotificationStore";
import { AttachmentNotifications } from "./AttachmentNotifications";

const Context = createContext<{ store: NotificationStore; attachments: AttachmentNotifications } | null>(null);

export function NotificationProvider({ store, children }: { store: NotificationStore; children: ReactNode }) {
  const value = useMemo(() => ({ store, attachments: new AttachmentNotifications(store) }), [store]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useNotificationEnvironment() {
  return useContext(Context);
}

export function useNotificationSource(source_key: string, input: NotificationInput | null) {
  const store = useNotificationEnvironment()?.store;
  useEffect(() => { store?.report(source_key, input); });
}
