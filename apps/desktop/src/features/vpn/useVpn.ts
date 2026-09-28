import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/errors";
import {
  connectVpn,
  deleteVpnConnection,
  loadVpnConnections,
  saveVpnConnection,
  stopVpn,
  vpnStatus,
} from "../../lib/tauri";
import type {
  VpnConnection,
  VpnConnectionInput,
  VpnConnectionsSnapshot,
  VpnStatus,
} from "../../lib/types";

export const VPN_STATUS_INTERVAL_MS = 5_000;
const STOPPED: VpnStatus = {
  endpoint: null,
  container_name: null,
  connection_id: null,
  running: false,
  state: "stopped",
};

export interface VpnEditor {
  editor_id: number;
  connection: VpnConnection | null;
  expected_revision: string | null;
}

export interface VpnAction {
  kind: "connect" | "stop";
  connection_id: string | null;
}

interface PendingRefresh {
  generation: number;
  pending: Promise<void>;
}

export interface VpnController {
  connections: readonly VpnConnection[];
  catalog_loaded: boolean;
  catalog_loading: boolean;
  catalog_error: string | null;
  status: VpnStatus;
  status_loaded: boolean;
  status_loading: boolean;
  status_stale: boolean;
  status_error: string | null;
  last_checked_at: number | null;
  action: VpnAction | null;
  action_error: string | null;
  profile_busy: boolean;
  deleting_id: string | null;
  editor: VpnEditor | null;
  editor_error: string | null;
  editor_saving: boolean;
  refresh(): Promise<void>;
  connect(connection_id: string): Promise<void>;
  stop(): Promise<void>;
  addConnection(): void;
  editConnection(connection: VpnConnection): void;
  closeEditor(): void;
  saveConnection(connection: VpnConnectionInput): Promise<boolean>;
  deleteConnection(connection_id: string): Promise<void>;
}

/** Observe ctld's VPN. Component lifetime never owns or stops the connection. */
export function useVpn(visible: boolean): VpnController {
  const [catalog, setCatalog] = useState<VpnConnectionsSnapshot>({ revision: null, connections: [] });
  const [catalog_loaded, setCatalogLoaded] = useState(false);
  const [catalog_loading, setCatalogLoading] = useState(false);
  const [catalog_error, setCatalogError] = useState<string | null>(null);
  const [status, setStatus] = useState<VpnStatus>(STOPPED);
  const [status_loaded, setStatusLoaded] = useState(false);
  const [status_loading, setStatusLoading] = useState(false);
  const [status_stale, setStatusStale] = useState(false);
  const [status_error, setStatusError] = useState<string | null>(null);
  const [last_checked_at, setLastCheckedAt] = useState<number | null>(null);
  const [action, setAction] = useState<VpnAction | null>(null);
  const [action_error, setActionError] = useState<string | null>(null);
  const [profile_busy, setProfileBusy] = useState(false);
  const [deleting_id, setDeletingId] = useState<string | null>(null);
  const [editor, setEditor] = useState<VpnEditor | null>(null);
  const [editor_error, setEditorError] = useState<string | null>(null);
  const [editor_saving, setEditorSaving] = useState(false);
  const mounted = useRef(false);
  const catalog_ref = useRef(catalog);
  const catalog_loaded_ref = useRef(false);
  const catalog_generation = useRef(0);
  const catalog_request = useRef<PendingRefresh | null>(null);
  const status_ref = useRef(status);
  const status_loaded_ref = useRef(false);
  const status_stale_ref = useRef(false);
  const status_generation = useRef(0);
  const status_request = useRef<PendingRefresh | null>(null);
  const action_ref = useRef<VpnAction | null>(null);
  const action_generation = useRef(0);
  const profile_busy_ref = useRef(false);
  const editor_ref = useRef<VpnEditor | null>(null);
  const editor_generation = useRef(0);

  const publishCatalog = useCallback((next: VpnConnectionsSnapshot) => {
    catalog_ref.current = next;
    catalog_loaded_ref.current = true;
    if (!mounted.current) return;
    setCatalog(next);
    setCatalogLoaded(true);
    setCatalogError(null);
  }, []);

  const publishStatus = useCallback((next: VpnStatus) => {
    status_ref.current = next;
    status_loaded_ref.current = true;
    status_stale_ref.current = false;
    if (!mounted.current) return;
    setStatus(next);
    setStatusLoaded(true);
    setStatusStale(false);
    setStatusError(null);
    setLastCheckedAt(Date.now());
  }, []);

  const refreshCatalog = useCallback(async () => {
    if (!mounted.current || profile_busy_ref.current) return;
    if (catalog_request.current?.generation === catalog_generation.current) return catalog_request.current.pending;
    const generation = ++catalog_generation.current;
    setCatalogLoading(true);
    const pending = (async () => {
      try {
        const next = await loadVpnConnections();
        if (mounted.current && generation === catalog_generation.current) publishCatalog(next);
      } catch (failure) {
        if (mounted.current && generation === catalog_generation.current) setCatalogError(errorMessage(failure));
      } finally {
        if (mounted.current && generation === catalog_generation.current) setCatalogLoading(false);
        if (catalog_request.current?.generation === generation) catalog_request.current = null;
      }
    })();
    catalog_request.current = { generation, pending };
    return pending;
  }, [publishCatalog]);

  const refreshStatus = useCallback(async () => {
    if (!mounted.current || action_ref.current) return;
    if (status_request.current?.generation === status_generation.current) return status_request.current.pending;
    const generation = ++status_generation.current;
    setStatusLoading(true);
    const pending = (async () => {
      try {
        const next = await vpnStatus();
        if (mounted.current && generation === status_generation.current) publishStatus(next);
      } catch (failure) {
        if (mounted.current && generation === status_generation.current) {
          status_stale_ref.current = true;
          setStatusStale(true);
          setStatusError(errorMessage(failure));
        }
      } finally {
        if (mounted.current && generation === status_generation.current) setStatusLoading(false);
        if (status_request.current?.generation === generation) status_request.current = null;
      }
    })();
    status_request.current = { generation, pending };
    return pending;
  }, [publishStatus]);

  const refresh = useCallback(async () => {
    await Promise.all([refreshCatalog(), refreshStatus()]);
  }, [refreshCatalog, refreshStatus]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      catalog_generation.current += 1;
      status_generation.current += 1;
      action_generation.current += 1;
    };
  }, []);

  useEffect(() => {
    if (!visible) return;
    void refresh();
    const onFocus = () => void refresh();
    const onVisibility = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void refreshStatus();
    }, VPN_STATUS_INTERVAL_MS);
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [visible, refresh, refreshStatus]);

  const isActive = useCallback((connection_id: string) =>
    (status_ref.current.state !== "stopped" && status_ref.current.connection_id === connection_id) ||
    (action_ref.current?.kind === "connect" && action_ref.current.connection_id === connection_id), []);

  const connect = useCallback(async (connection_id: string) => {
    if (!mounted.current || action_ref.current || !status_loaded_ref.current ||
      status_stale_ref.current || status_ref.current.state !== "stopped") return;
    if (!catalog_ref.current.connections.some((connection) => connection.connection_id === connection_id)) return;
    const generation = ++action_generation.current;
    status_generation.current += 1;
    const previous = status_ref.current;
    const next_action: VpnAction = { kind: "connect", connection_id };
    action_ref.current = next_action;
    setAction(next_action);
    setActionError(null);
    setStatusLoading(false);
    status_ref.current = { ...STOPPED, connection_id, state: "starting" };
    setStatus(status_ref.current);
    try {
      const next = await connectVpn(connection_id);
      if (mounted.current && generation === action_generation.current) {
        status_generation.current += 1;
        publishStatus(next);
      }
    } catch (failure) {
      if (mounted.current && generation === action_generation.current) {
        status_ref.current = previous;
        setStatus(previous);
        status_stale_ref.current = true;
        setStatusStale(true);
        setActionError(errorMessage(failure));
      }
    } finally {
      if (mounted.current && generation === action_generation.current) {
        action_ref.current = null;
        setAction(null);
        void refreshStatus();
      }
    }
  }, [publishStatus, refreshStatus]);

  const stop = useCallback(async () => {
    if (!mounted.current || action_ref.current?.kind === "stop") return;
    if (!action_ref.current && status_ref.current.state === "stopped" && !status_stale_ref.current) return;
    const generation = ++action_generation.current;
    status_generation.current += 1;
    const previous = status_ref.current;
    const next_action: VpnAction = { kind: "stop", connection_id: previous.connection_id };
    action_ref.current = next_action;
    setAction(next_action);
    setActionError(null);
    setStatusLoading(false);
    status_ref.current = { ...previous, running: false, state: "stopping" };
    setStatus(status_ref.current);
    try {
      const next = await stopVpn();
      if (mounted.current && generation === action_generation.current) {
        status_generation.current += 1;
        publishStatus(next);
      }
    } catch (failure) {
      if (mounted.current && generation === action_generation.current) {
        status_ref.current = previous;
        setStatus(previous);
        status_stale_ref.current = true;
        setStatusStale(true);
        setActionError(errorMessage(failure));
      }
    } finally {
      if (mounted.current && generation === action_generation.current) {
        action_ref.current = null;
        setAction(null);
        void refreshStatus();
      }
    }
  }, [publishStatus, refreshStatus]);

  const addConnection = useCallback(() => {
    if (profile_busy_ref.current || !catalog_loaded_ref.current) return;
    const next: VpnEditor = {
      editor_id: ++editor_generation.current,
      connection: null,
      expected_revision: catalog_ref.current.revision,
    };
    editor_ref.current = next;
    setEditor(next);
    setEditorError(null);
  }, []);

  const editConnection = useCallback((connection: VpnConnection) => {
    if (profile_busy_ref.current || isActive(connection.connection_id)) return;
    const current = catalog_ref.current.connections.find((item) => item.connection_id === connection.connection_id);
    if (!current) return;
    const next: VpnEditor = {
      editor_id: ++editor_generation.current,
      connection: current,
      expected_revision: catalog_ref.current.revision,
    };
    editor_ref.current = next;
    setEditor(next);
    setEditorError(null);
  }, [isActive]);

  const closeEditor = useCallback(() => {
    if (profile_busy_ref.current) return;
    editor_ref.current = null;
    setEditor(null);
    setEditorError(null);
  }, []);

  const saveConnection = useCallback(async (connection: VpnConnectionInput) => {
    const selected = editor_ref.current;
    if (!mounted.current || !selected || profile_busy_ref.current) return false;
    if (isActive(connection.connection_id)) {
      setEditorError("Disconnect this VPN before changing its settings.");
      return false;
    }
    profile_busy_ref.current = true;
    catalog_generation.current += 1;
    setProfileBusy(true);
    setCatalogLoading(false);
    setEditorSaving(true);
    setEditorError(null);
    try {
      const next = await saveVpnConnection(selected.expected_revision, connection);
      if (!mounted.current) return true;
      catalog_generation.current += 1;
      publishCatalog(next);
      if (editor_ref.current?.editor_id === selected.editor_id) {
        editor_ref.current = null;
        setEditor(null);
      }
      return true;
    } catch (failure) {
      if (mounted.current && editor_ref.current?.editor_id === selected.editor_id) setEditorError(errorMessage(failure));
      return false;
    } finally {
      profile_busy_ref.current = false;
      if (mounted.current) {
        setProfileBusy(false);
        setEditorSaving(false);
      }
    }
  }, [isActive, publishCatalog]);

  const deleteConnection = useCallback(async (connection_id: string) => {
    if (!mounted.current || profile_busy_ref.current || isActive(connection_id)) return;
    profile_busy_ref.current = true;
    const revision = catalog_ref.current.revision;
    catalog_generation.current += 1;
    setProfileBusy(true);
    setCatalogLoading(false);
    setDeletingId(connection_id);
    setCatalogError(null);
    try {
      const next = await deleteVpnConnection(revision, connection_id);
      if (mounted.current) {
        catalog_generation.current += 1;
        publishCatalog(next);
      }
    } catch (failure) {
      if (mounted.current) setCatalogError(errorMessage(failure));
    } finally {
      profile_busy_ref.current = false;
      if (mounted.current) {
        setProfileBusy(false);
        setDeletingId(null);
      }
    }
  }, [isActive, publishCatalog]);

  return {
    connections: catalog.connections,
    catalog_loaded, catalog_loading, catalog_error,
    status, status_loaded, status_loading, status_stale, status_error, last_checked_at,
    action, action_error, profile_busy, deleting_id, editor, editor_error, editor_saving,
    refresh, connect, stop, addConnection, editConnection, closeEditor, saveConnection, deleteConnection,
  };
}
