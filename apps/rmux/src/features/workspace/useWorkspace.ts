import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { loadWorkspace, updateWorkspace } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import {
  browserStorage,
  clearLegacyRemoteTargets,
  readLegacyRemoteTargets,
} from "../targets/targets";
import { WorkspaceWriter } from "./WorkspaceWriter";
import {
  emptyWorkspaceView,
  restoreWorkspace,
  withHostId,
  workspaceDocument,
  type WorkspaceView,
} from "./workspaceModel";

export function useWorkspace() {
  const [view, setView] = useState(emptyWorkspaceView);
  const viewRef = useRef(view);
  const writerRef = useRef<WorkspaceWriter | null>(null);
  const workspaceIdRef = useRef("default");
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    let cancelled = false;
    void (async () => {
      try {
        const snapshot = await loadWorkspace();
        if (cancelled) return;
        let restored = restoreWorkspace(snapshot.document);
        const writer = new WorkspaceWriter(snapshot, updateWorkspace);
        if (snapshot.revision === null) {
          const legacy = readLegacyRemoteTargets(browserStorage());
          restored = {
            ...restored,
            targets: [...restored.targets, ...legacy.map(withHostId)],
          };
          await writer.write(
            workspaceDocument(restored, snapshot.document.workspace_id),
          );
          if (cancelled) return;
          // Keep the legacy copy until the native migration is durably saved.
          clearLegacyRemoteTargets(browserStorage());
        }
        writerRef.current = writer;
        workspaceIdRef.current = snapshot.document.workspace_id;
        viewRef.current = restored;
        setView(restored);
        setReady(true);
      } catch (failure) {
        if (!cancelled) setError(errorMessage(failure));
      }
    })();
    return () => {
      cancelled = true;
      mounted.current = false;
    };
  }, []);

  const persist = useCallback((retry = false): Promise<void> => {
    const writer = writerRef.current;
    if (!writer) return Promise.reject(new Error("Workspace is not loaded."));
    return writer
      .write(workspaceDocument(viewRef.current, workspaceIdRef.current), retry)
      .then(
        () => {
          if (mounted.current) setError(null);
        },
        (failure: unknown) => {
          if (mounted.current) setError(errorMessage(failure));
          throw failure;
        },
      );
  }, []);

  const update = useCallback(
    <K extends keyof WorkspaceView>(
      key: K,
      action: SetStateAction<WorkspaceView[K]>,
    ) => {
      if (!writerRef.current) return;
      const previous = viewRef.current;
      const value =
        typeof action === "function"
          ? (action as (current: WorkspaceView[K]) => WorkspaceView[K])(
              previous[key],
            )
          : action;
      if (value === previous[key]) return;
      viewRef.current = { ...previous, [key]: value };
      setView(viewRef.current);
      void persist().catch(() => undefined);
    },
    [persist],
  );

  // Normal window close waits for queued disk writes; failed saves remain visible.
  useEffect(() => {
    if (!ready || !("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    let closing = false;
    const registration = getCurrentWindow().onCloseRequested(async (event) => {
      event.preventDefault();
      if (closing) return;
      closing = true;
      try {
        await persist(true);
        await getCurrentWindow().destroy();
      } catch {
        closing = false;
      }
    });
    void registration.then((unlisten) => {
      if (disposed) unlisten();
    });
    return () => {
      disposed = true;
      void registration.then((unlisten) => unlisten());
    };
  }, [ready, persist]);

  const setTargets = useCallback(
    (action: SetStateAction<WorkspaceView["targets"]>) =>
      update("targets", action),
    [update],
  );
  const setSessions = useCallback(
    (action: SetStateAction<WorkspaceView["sessions"]>) =>
      update("sessions", action),
    [update],
  );
  const setTabs = useCallback(
    (action: SetStateAction<WorkspaceView["tabs"]>) => update("tabs", action),
    [update],
  );
  const setActiveTabKey = useCallback(
    (action: SetStateAction<WorkspaceView["active_tab_key"]>) =>
      update("active_tab_key", action),
    [update],
  );
  const setShellStates = useCallback(
    (action: SetStateAction<WorkspaceView["shell_states"]>) =>
      update("shell_states", action),
    [update],
  );

  return {
    ...view,
    viewRef,
    ready,
    error,
    persist,
    setTargets,
    setSessions,
    setTabs,
    setActiveTabKey,
    setShellStates,
  };
}
