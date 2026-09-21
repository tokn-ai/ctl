import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { loadHosts, loadWorkspace, listSshConfigHosts, updateHosts, updateWorkspace } from "../../lib/tauri";
import type { HostCatalogDocument, SshConfigHost } from "../../lib/types";
import { errorMessage } from "../../lib/errors";
import {
  browserStorage,
  clearLegacyRemoteTargets,
  readLegacyRemoteTargets,
} from "../targets/targets";
import { sessionKey } from "../targets/targets";
import { WorkspaceWriter } from "./WorkspaceWriter";
import { sameSshEndpoint } from "./remoteRecovery";
import {
  emptyWorkspaceView,
  restoreWorkspace,
  withHostId,
  hostFromTarget,
  hostCatalogDocument,
  refreshHostCatalog,
  workspaceDocument,
  workspaceTabKey,
  type WorkspaceView,
} from "./workspaceModel";

export function useWorkspace() {
  const [view, setView] = useState(emptyWorkspaceView);
  const viewRef = useRef(view);
  const writerRef = useRef<WorkspaceWriter | null>(null);
  const hostWriterRef = useRef<WorkspaceWriter<HostCatalogDocument> | null>(null);
  const savedCatalogRef = useRef<HostCatalogDocument>({ schema_version: 1, hosts: [], ssh_gateways: [] });
  const [sshConfigHosts, setSshConfigHosts] = useState<SshConfigHost[]>([]);
  const [sshConfigWarning, setSshConfigWarning] = useState<string | null>(null);
  const workspaceIdRef = useRef("default");
  const [ready, setReady] = useState(false);
  const pendingWrites = useRef(0);
  const writeQueue = useRef<Promise<void>>(Promise.resolve());
  const [saving, setSaving] = useState(false);
  const [closing, setClosing] = useState(false);
  const closingRef = useRef(false);
  const isClosing = useCallback(() => closingRef.current, []);
  const closeBlockedRef = useRef<() => boolean>(() => false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    let cancelled = false;
    void (async () => {
      try {
        const [snapshot, catalog, ssh] = await Promise.all([
          loadWorkspace(),
          loadHosts(),
          listSshConfigHosts().catch((failure: unknown) => ({ hosts: [], warnings: [errorMessage(failure)] })),
        ]);
        if (cancelled) return;
        let restored = restoreWorkspace(snapshot.document, catalog.document, ssh.hosts);
        const writer = new WorkspaceWriter(snapshot, updateWorkspace);
        // Normalize native field order before comparing serialized snapshots.
        let savedCatalog = hostCatalogDocument(restored);
        const hostWriter = new WorkspaceWriter({ ...catalog, document: savedCatalog }, updateHosts);
        if (snapshot.revision === null) {
          const legacy = readLegacyRemoteTargets(browserStorage());
          const additions = legacy.filter((target) => !restored.hosts.some((host) =>
            (!host.source || host.source === "saved") && host.connection_methods.some((method) =>
              sameSshEndpoint(method.target, target))));
          restored = {
            ...restored,
            targets: [...restored.targets, ...additions.map(withHostId)],
          };
          restored.hosts = restored.targets.map((target) => restored.hosts.find((host) =>
            host.host_id === (target.kind === "local" ? "local" : target.host_id)) ?? hostFromTarget(target));
          const imported = hostCatalogDocument(restored);
          if (JSON.stringify(imported) !== JSON.stringify(savedCatalog)) {
            await hostWriter.write(imported);
            savedCatalog = imported;
          }
          if (legacy.length > 0) await writer.write(
            workspaceDocument(restored, snapshot.document.workspace_id),
          );
          if (cancelled) return;
          // Keep the legacy copy until the native migration is durably saved.
          clearLegacyRemoteTargets(browserStorage());
        }
        writerRef.current = writer;
        hostWriterRef.current = hostWriter;
        savedCatalogRef.current = savedCatalog;
        setSshConfigHosts(ssh.hosts);
        setSshConfigWarning(ssh.warnings.join("\n") || null);
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

  const queueOperation = useCallback((run: (writer: WorkspaceWriter) => Promise<void>, mode: "write" | "read" = "write"): Promise<void> => {
    const writer = writerRef.current;
    if (!writer) return Promise.reject(new Error("Workspace is not loaded."));
    if (mode === "write") {
      pendingWrites.current += 1;
      setSaving(true);
    }
    const operation = writeQueue.current
      .catch(() => undefined)
      .then(() => run(writer))
      .then(
        () => {
          if (mode === "write" && mounted.current) setError(null);
        },
        (failure: unknown) => {
          if (mode === "write" && mounted.current) setError(errorMessage(failure));
          throw failure;
        },
      )
      .finally(() => {
        if (mode === "write") {
          pendingWrites.current -= 1;
          if (mounted.current && pendingWrites.current === 0) setSaving(false);
        }
      });
    writeQueue.current = operation;
    return operation;
  }, []);

  const persistCatalog = useCallback(async (next: WorkspaceView, retry = false) => {
    const document = hostCatalogDocument(next);
    if (JSON.stringify(document) === JSON.stringify(savedCatalogRef.current)) return false;
    const writer = hostWriterRef.current;
    if (!writer) throw new Error("Hosts are not loaded.");
    await writer.write(document, retry);
    savedCatalogRef.current = document;
    return true;
  }, []);

  const persist = useCallback((retry = false): Promise<void> =>
    queueOperation(async (writer) => {
      await persistCatalog(viewRef.current, retry);
      await writer.write(workspaceDocument(viewRef.current, workspaceIdRef.current), retry);
    }), [queueOperation, persistCatalog]);

  const replaceView = useCallback((replace: (current: WorkspaceView) => WorkspaceView) => {
    if (!writerRef.current || closingRef.current) throw new Error("Workspace is not available.");
    return queueOperation(async (writer) => {
      const proposed = replace(viewRef.current);
      const catalogChanged = await persistCatalog(proposed, true);
      if (!catalogChanged) {
        await writer.write(workspaceDocument(proposed, workspaceIdRef.current), true);
      }
      // The catalog commits independently of workspace observations. Once a
      // host is saved, publish it even if a later workspace autosave fails.
      const committed = replace(viewRef.current);
      viewRef.current = committed;
      if (mounted.current) setView(committed);
      if (catalogChanged) void persist().catch(() => undefined);
    });
  }, [queueOperation, persistCatalog, persist]);

  const refreshSshConfig = useCallback(async () => {
    try {
      const catalog = await listSshConfigHosts();
      if (mounted.current) {
        setSshConfigHosts(catalog.hosts);
        setSshConfigWarning(catalog.warnings.join("\n") || null);
      }
      if (!writerRef.current || closingRef.current) return;
      // Keep discovery behind pending catalog commits. Removing an alias while
      // its promotion is saving must not erase the newly saved host.
      await queueOperation(async () => {
        const next = refreshHostCatalog(viewRef.current, hostCatalogDocument(viewRef.current), catalog.hosts);
        viewRef.current = next;
        if (mounted.current) setView(next);
      }, "read");
    } catch (failure) {
      if (mounted.current) setSshConfigWarning(errorMessage(failure));
    }
  }, [queueOperation]);

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
    sshConfigHosts,
    sshConfigWarning,
    refreshSshConfig,
    ready,
    saving,
    closing,
    isClosing,
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
