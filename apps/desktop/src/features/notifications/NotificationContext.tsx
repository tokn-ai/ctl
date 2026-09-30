import { createContext, useContext, useEffect, type ReactNode } from "react";
import type { NotificationInput } from "../../lib/types";
import type { NotificationStore } from "./NotificationStore";

const Context = createContext<NotificationStore | null>(null);

export function NotificationProvider({ store, children }: { store: NotificationStore; children: ReactNode }) {
  return <Context.Provider value={store}>{children}</Context.Provider>;
}

export function useNotificationSource(source_key: string, input: NotificationInput | null) {
  const store = useContext(Context);
  useEffect(() => { store?.report(source_key, input); });
}
