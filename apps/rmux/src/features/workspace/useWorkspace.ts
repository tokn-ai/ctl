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
import { sessionKey } from "../targets/targets";
import { WorkspaceWriter } from "./WorkspaceWriter";
import {
  emptyWorkspaceView,
  restoreWorkspace,
  withHostId,
  hostFromTarget,
  workspaceDocument,
  workspaceTabKey,
  type WorkspaceView,
} from "./workspaceModel";

export function useWorkspace() {
  const [view, setView] = useState(emptyWorkspaceView);
  const viewRef = useRef(view);
  const writerRef = useRef<WorkspaceWriter | null>(null);
  const workspaceIdRef = useRef("default");
  const [ready, setReady] = useState(false);
  const pendingWrites = useRef(0);
  const writeQueue = useRef<Promise<void>>(Promise.resolve());
  const [saving, setSaving] = useState(false);
  const [closing, setClosing] = useState(false);
  const closingRef = useRef(false);
  const closeBlockedRef = useRef<() => boolean>(() => false);
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
          restored.hosts = restored.targets.map((target) => hostFromTarget(target));
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

  const queueWrite = useCallback((write: (writer: WorkspaceWriter) => Promise<void>): Promise<void> => {
    const writer = writerRef.current;
    if (!writer) return Promise.reject(new Error("Workspace is not loaded."));
    pendingWrites.current += 1;
    setSaving(true);
    const operation = writeQueue.current
      .catch(() => undefined)
      .then(() => write(writer))
      .then(
        () => {
          if (mounted.current) setError(null);
        },
        (failure: unknown) => {
          if (mounted.current) setError(errorMessage(failure));
          throw failure;
        },
      )
      .finally(() => {
        pendingWrites.current -= 1;
        if (mounted.current && pendingWrites.current === 0) setSaving(false);
      });
    writeQueue.current = operation;
    return operation;
  }, []);

  const persist = useCallback((retry = false): Promise<void> =>
    queueWrite((writer) => writer.write(
      workspaceDocument(viewRef.current, workspaceIdRef.current), retry,
    )), [queueWrite]);

  const replaceView = useCallback((replace: (current: WorkspaceView) => WorkspaceView) => {
    if (!writerRef.current || closingRef.current) throw new Error("Workspace is not available.");
    return queueWrite(async (writer) => {
      const proposed = replace(viewRef.current);
      await writer.write(workspaceDocument(proposed, workspaceIdRef.current), true);
      // Autosaves queue behind this write. Reapply the host change to the latest
      // view so observations made while saving retain their state and metadata.
      const committed = replace(viewRef.current);
      viewRef.current = committed;
      if (mounted.current) setView(committed);
    });
  }, [queueWrite]);

  const update = useCallback(
    <K extends keyof WorkspaceView>(
      key: K,
      action: SetStateAction<WorkspaceView[K]>,
    ) => {
      if (!writerRef.current || closingRef.current) return;
      const previous = viewRef.current;
      const value =
        typeof action === "function"
          ? (action as (current: WorkspaceView[K]) => WorkspaceView[K])(
              previous[key],
            )
          : action;
      if (value === previous[key]) return;
      const next = { ...previous, [key]: value };
      if (key === "targets") {
        next.hosts = next.targets.map((target) => {
          const id = target.kind === "local" ? "local" : target.host_id;
          return previous.hosts.find((host) => host.host_id === id) ?? hostFromTarget(target);
        });
      }
      if (key === "tabs" || key === "task_tabs") {
        const keys = [
          ...next.tabs.map(sessionKey),
          ...next.task_tabs.map(workspaceTabKey),
        ];
        next.tab_order = [
          ...previous.tab_order.filter((item) => keys.includes(item)),
          ...keys.filter((item) => !previous.tab_order.includes(item)),
        ];
      }
      viewRef.current = next;
      setView(viewRef.current);
      void persist().catch(() => undefined);
    },
    [persist],
  );

  // Normal window close waits for queued disk writes; failed saves remain visible.
  useEffect(() => {
    if (!ready || !("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    const registration = getCurrentWindow().onCloseRequested(async (event) => {
      event.preventDefault();
      if (closingRef.current) return;
      if (closeBlockedRef.current()) {
        setError("Wait for ongoing operations before closing the window.");
        return;
      }
      closingRef.current = true;
      setClosing(true);
      try {
        await persist(true);
        await getCurrentWindow().destroy();
      } catch (failure) {
        closingRef.current = false;
        setClosing(false);
        setError(`Window close paused: ${errorMessage(failure)}`);
      }
    });
    void registration.then(
      (unlisten) => {
        if (disposed) unlisten();
      },
      (failure: unknown) => {
        if (!disposed) setError(errorMessage(failure));
      },
    );
    return () => {
      disposed = true;
      void registration.then(
        (unlisten) => unlisten(),
        () => undefined,
      );
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
    replaceView,
    ready,
    saving,
    closing,
    closeBlockedRef,
    error,
    persist,
    update,
    setTargets,
    setSessions,
    setTabs,
    setActiveTabKey,
    setShellStates,
  };
}
