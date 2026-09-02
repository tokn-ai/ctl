import { useCallback, useEffect, useRef } from "react";
import type { ConnectionTarget, SessionSummary } from "../../lib/types";
import { sameTarget, sessionKey } from "../targets/targets";

interface WorkspaceConnections {
  ready: boolean;
  closing: boolean;
  tabs: readonly SessionSummary[];
  active_tab_key: string | null;
  activateTab(session: SessionSummary): Promise<void>;
  refreshHost(target: ConnectionTarget): Promise<void>;
}

/** Connection policy lives above storage: loading a document is still pure I/O. */
export function useWorkspaceConnections(options: WorkspaceConnections) {
  const current = useRef(options);
  current.current = options;
  const restored = useRef(false);
  const mounted = useRef(false);
  const hostConnection = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      hostConnection.current += 1;
    };
  }, []);

  useEffect(() => {
    if (!options.ready || options.closing || restored.current) return;
    restored.current = true;
    const selected = options.tabs.find(
      (tab) => sessionKey(tab) === options.active_tab_key,
    );
    // One startup intent, not an effect that retries errors or reopens closed tabs.
    // The attachment controller defers it until the renderer is ready.
    if (selected?.target.kind === "local") void options.activateTab(selected);
  }, [options]);

  return useCallback(async (target: ConnectionTarget) => {
    const before = current.current;
    if (!before.ready || before.closing) return;
    const attempt = ++hostConnection.current;
    const hostTabs = before.tabs.filter((tab) =>
      sameTarget(tab.target, target),
    );
    const selected =
      hostTabs.find((tab) => sessionKey(tab) === before.active_tab_key) ??
      hostTabs[0];

    // Finish inspection first so attaching cannot invalidate its observations.
    await before.refreshHost(target);

    const after = current.current;
    if (
      !selected ||
      !mounted.current ||
      hostConnection.current !== attempt ||
      !after.ready ||
      after.closing ||
      after.active_tab_key !== before.active_tab_key
    )
      return;
    const tab = after.tabs.find(
      (candidate) => sessionKey(candidate) === sessionKey(selected),
    );
    // A late probe/inspection must not resurrect a closed tab or removed host.
    if (tab) await after.activateTab(tab);
  }, []);
}
