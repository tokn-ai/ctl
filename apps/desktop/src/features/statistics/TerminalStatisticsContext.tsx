import { createContext, useContext, useEffect, useState, useSyncExternalStore, type ReactNode } from "react";
import type { SessionSummary } from "../../lib/types";
import { TerminalStatisticsStore } from "./TerminalStatisticsStore";

const StatisticsContext = createContext<TerminalStatisticsStore | null>(null);

export function TerminalStatisticsProvider({ children, store: supplied }: { children: ReactNode; store?: TerminalStatisticsStore }) {
  const [store] = useState(() => supplied ?? new TerminalStatisticsStore());
  useEffect(() => {
    const timer = setInterval(() => store.tick(), 500);
    return () => clearInterval(timer);
  }, [store]);
  return <StatisticsContext.Provider value={store}>{children}</StatisticsContext.Provider>;
}

export function useTerminalStatisticsStore(): TerminalStatisticsStore | null {
  return useContext(StatisticsContext);
}

const subscribeNone = () => () => {};
const emptyVersion = () => 0;

export function useTerminalStatistics(session: SessionSummary | null) {
  const store = useTerminalStatisticsStore();
  useSyncExternalStore(store?.subscribe ?? subscribeNone, store?.version ?? emptyVersion);
  return store?.snapshot(session) ?? null;
}
