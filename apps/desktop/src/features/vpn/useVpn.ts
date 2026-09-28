import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/errors";
import { connectVpn, deleteVpnConnection, loadVpnConnections, openVpnSignIn, saveVpnConnection, stopVpn, vpnStatus } from "../../lib/tauri";
import type { VpnConnection, VpnConnectionInput, VpnConnectionsSnapshot, VpnProvider, VpnStatus } from "../../lib/types";
import { vpnNeedsSignIn, vpnRuntimeId } from "./status";

export const VPN_STATUS_INTERVAL_MS = 5_000;

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
  statuses: readonly VpnStatus[];
  supports_multiple: boolean;
  supported_providers: readonly VpnProvider[];
  signing_in_ids: ReadonlySet<string>;
  status_loaded: boolean;
  status_loading: boolean;
  status_stale: boolean;
  status_error: string | null;
  last_checked_at: number | null;
  actions: ReadonlyMap<string, VpnAction>;
  action_errors: ReadonlyMap<string, string>;
  uncertain_ids: ReadonlySet<string>;
  profile_busy: boolean;
  deleting_id: string | null;
  editor: VpnEditor | null;
  editor_error: string | null;
  editor_saving: boolean;
  refresh(): Promise<void>;
  connect(connection_id: string): Promise<void>;
  stop(vpn_id: string): Promise<void>;
  signIn(vpn_id: string): Promise<void>;
  addConnection(): void;
  editConnection(connection: VpnConnection): void;
  closeEditor(): void;
  saveConnection(connection: VpnConnectionInput): Promise<boolean>;
  deleteConnection(connection_id: string): Promise<void>;
}

/** Observe ctld while the workspace is ready. Component lifetime never owns or stops VPNs. */
export function useVpn(enabled: boolean): VpnController {
  const [catalog, setCatalog] = useState<VpnConnectionsSnapshot>({ revision: null, connections: [] });
  const [catalog_loaded, setCatalogLoaded] = useState(false);
  const [catalog_loading, setCatalogLoading] = useState(false);
  const [catalog_error, setCatalogError] = useState<string | null>(null);
  const [statuses, setStatuses] = useState<VpnStatus[]>([]);
  const [supports_multiple, setSupportsMultiple] = useState(false);
  const [supported_providers, setSupportedProviders] = useState<readonly VpnProvider[]>(["openconnect"]);
  const [signing_in_ids, setSigningInIds] = useState<ReadonlySet<string>>(new Set());
  const [status_loaded, setStatusLoaded] = useState(false);
  const [status_loading, setStatusLoading] = useState(false);
  const [status_stale, setStatusStale] = useState(false);
  const [status_error, setStatusError] = useState<string | null>(null);
  const [last_checked_at, setLastCheckedAt] = useState<number | null>(null);
  const [actions, setActions] = useState<ReadonlyMap<string, VpnAction>>(new Map());
  const [action_errors, setActionErrors] = useState<ReadonlyMap<string, string>>(new Map());
  const [uncertain_ids, setUncertainIds] = useState<ReadonlySet<string>>(new Set());
  const [profile_busy, setProfileBusy] = useState(false);
  const [deleting_id, setDeletingId] = useState<string | null>(null);
  const [editor, setEditor] = useState<VpnEditor | null>(null);
  const [editor_error, setEditorError] = useState<string | null>(null);
  const [editor_saving, setEditorSaving] = useState(false);
  const mounted = useRef(false);
  const lifetime = useRef(0);
  const catalog_ref = useRef(catalog);
  const catalog_loaded_ref = useRef(false);
  const catalog_generation = useRef(0);
  const catalog_request = useRef<PendingRefresh | null>(null);
  const statuses_ref = useRef(new Map<string, VpnStatus>());
  const supports_multiple_ref = useRef(false);
  const supported_providers_ref = useRef<readonly VpnProvider[]>(["openconnect"]);
  const signing_in_ref = useRef(new Set<string>());
  const sign_in_failed_ids = useRef(new Set<string>());
  const status_loaded_ref = useRef(false);
  const status_stale_ref = useRef(false);
  const status_generation = useRef(0);
  const status_request = useRef<PendingRefresh | null>(null);
  const actions_ref = useRef(new Map<string, VpnAction>());
  const action_errors_ref = useRef(new Map<string, string>());
  const uncertain_ids_ref = useRef(new Set<string>());
  const runtime_generations = useRef(new Map<string, number>());
  const failed_actions = useRef(new Map<string, VpnAction & { generation: number }>());
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

  const publishRuntimes = useCallback((next: Map<string, VpnStatus>) => {
    statuses_ref.current = next;
    if (mounted.current) setStatuses([...next.values()]);
  }, []);

  const publishRuntime = useCallback((vpn_id: string, next: VpnStatus | undefined) => {
    const updated = new Map(statuses_ref.current);
    updated.delete(vpn_id);
    if (next && next.state !== "stopped") updated.set(vpnRuntimeId(next), next);
    publishRuntimes(updated);
  }, [publishRuntimes]);

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
    if (!mounted.current) return;
    if (status_request.current?.generation === status_generation.current) return status_request.current.pending;
    const generation = ++status_generation.current;
    const observed_generations = new Map(runtime_generations.current);
    setStatusLoading(true);
    const pending = (async () => {
      try {
        const snapshot = await vpnStatus();
        if (!mounted.current || generation !== status_generation.current) return;
        const next = new Map(snapshot.connections.filter((status) => status.state !== "stopped").map((status) => [vpnRuntimeId(status), status]));
        // A whole-daemon observation must not undo newer or pending work on an individual VPN.
        for (const [vpn_id, current_generation] of runtime_generations.current) {
          if (observed_generations.get(vpn_id) !== current_generation || actions_ref.current.has(vpn_id)) {
            const current = statuses_ref.current.get(vpn_id);
            if (current) next.set(vpn_id, current);
            else next.delete(vpn_id);
            continue;
          }
          const failure = failed_actions.current.get(vpn_id);
          const observed = next.get(vpn_id);
          uncertain_ids_ref.current.delete(vpn_id);
          if (failure?.generation === current_generation && (
            (failure.kind === "stop" && !observed) ||
            (failure.kind === "connect" && observed?.state === "connected" && observed.connection_id === failure.connection_id)
          )) {
            failed_actions.current.delete(vpn_id);
            action_errors_ref.current.delete(vpn_id);
          }
        }
        for (const vpn_id of sign_in_failed_ids.current) {
          if (!vpnNeedsSignIn(next.get(vpn_id))) {
            sign_in_failed_ids.current.delete(vpn_id);
            action_errors_ref.current.delete(vpn_id);
          }
        }
        publishRuntimes(next);
        supports_multiple_ref.current = snapshot.supports_multiple;
        supported_providers_ref.current = snapshot.supported_providers ?? ["openconnect"];
        setSupportedProviders(supported_providers_ref.current);
        status_loaded_ref.current = true;
        status_stale_ref.current = false;
        setSupportsMultiple(snapshot.supports_multiple);
        setStatusLoaded(true);
        setStatusStale(false);
        setStatusError(null);
        setLastCheckedAt(Date.now());
        setActionErrors(new Map(action_errors_ref.current));
        setUncertainIds(new Set(uncertain_ids_ref.current));
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
  }, [publishRuntimes]);

  const refresh = useCallback(async () => {
    await Promise.all([refreshCatalog(), refreshStatus()]);
  }, [refreshCatalog, refreshStatus]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      lifetime.current += 1;
      catalog_generation.current += 1;
      status_generation.current += 1;
    };
  }, []);

  useEffect(() => {
    if (!enabled) return;
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
  }, [enabled, refresh, refreshStatus]);

  const isActive = useCallback((connection_id: string) =>
    uncertain_ids_ref.current.has(connection_id) ||
    [...statuses_ref.current.values()].some((status) => status.connection_id === connection_id) ||
    [...actions_ref.current.values()].some((action) => action.connection_id === connection_id), []);

  const runAction = useCallback(async (vpn_id: string, action: VpnAction, optimistic: VpnStatus, operation: () => Promise<VpnStatus>) => {
    const generation = (runtime_generations.current.get(vpn_id) ?? 0) + 1;
    const started_lifetime = lifetime.current;
    runtime_generations.current.set(vpn_id, generation);
    const previous = statuses_ref.current.get(vpn_id);
    actions_ref.current.set(vpn_id, action);
    action_errors_ref.current.delete(vpn_id);
    sign_in_failed_ids.current.delete(vpn_id);
    uncertain_ids_ref.current.delete(vpn_id);
    failed_actions.current.delete(vpn_id);
    setActions(new Map(actions_ref.current));
    setActionErrors(new Map(action_errors_ref.current));
    setUncertainIds(new Set(uncertain_ids_ref.current));
    publishRuntime(vpn_id, optimistic);
    const current = () => mounted.current && started_lifetime === lifetime.current && runtime_generations.current.get(vpn_id) === generation;
    try {
      const next = await operation();
      if (current()) publishRuntime(vpn_id, next);
    } catch (failure) {
      if (current()) {
        publishRuntime(vpn_id, previous);
        failed_actions.current.set(vpn_id, { ...action, generation });
        action_errors_ref.current.set(vpn_id, errorMessage(failure));
        uncertain_ids_ref.current.add(vpn_id);
        setActionErrors(new Map(action_errors_ref.current));
        setUncertainIds(new Set(uncertain_ids_ref.current));
      }
    } finally {
      if (current()) {
        // Fence observations that began before this result, including polls during this action.
        const completed_generation = generation + 1;
        runtime_generations.current.set(vpn_id, completed_generation);
        const failure = failed_actions.current.get(vpn_id);
        if (failure) failure.generation = completed_generation;
        actions_ref.current.delete(vpn_id);
        setActions(new Map(actions_ref.current));
        status_generation.current += 1;
        void refreshStatus();
      }
    }
  }, [publishRuntime, refreshStatus]);

  const connect = useCallback(async (connection_id: string) => {
    if (!mounted.current || !status_loaded_ref.current || status_stale_ref.current || isActive(connection_id)) return;
    if (!supports_multiple_ref.current && (statuses_ref.current.size > 0 || actions_ref.current.size > 0 || uncertain_ids_ref.current.size > 0)) return;
    const connection = catalog_ref.current.connections.find((item) => item.connection_id === connection_id);
    if (!connection || !supported_providers_ref.current.includes(connection.provider ?? "openconnect")) return;
    await runAction(connection_id, { kind: "connect", connection_id }, {
      vpn_id: connection_id, connection_id, state: "starting", running: false,
      provider: connection.provider ?? "openconnect",
      ...(connection.provider === "tailscale" ? { hostname: connection.hostname } : { vpn_url: connection.url, username: connection.username }),
      endpoint: null, container_name: null,
    }, () => connectVpn(connection_id));
  }, [isActive, runAction]);

  const stop = useCallback(async (vpn_id: string) => {
    if (!mounted.current || !supports_multiple_ref.current || actions_ref.current.get(vpn_id)?.kind === "stop") return;
    const previous = statuses_ref.current.get(vpn_id);
    if (!previous && !uncertain_ids_ref.current.has(vpn_id)) return;
    const connection_id = previous?.connection_id ?? (catalog_ref.current.connections.some((connection) => connection.connection_id === vpn_id) ? vpn_id : null);
    await runAction(vpn_id, { kind: "stop", connection_id }, {
      endpoint: null, container_name: null, ...previous, connection_id, vpn_id, state: "stopping", running: false,
    }, () => stopVpn(vpn_id));
  }, [runAction]);

  const signIn = useCallback(async (vpn_id: string) => {
    if (!mounted.current || status_stale_ref.current || signing_in_ref.current.has(vpn_id) ||
      actions_ref.current.get(vpn_id)?.kind === "stop" || !vpnNeedsSignIn(statuses_ref.current.get(vpn_id))) return;
    const generation = runtime_generations.current.get(vpn_id);
    const started_lifetime = lifetime.current;
    signing_in_ref.current.add(vpn_id);
    action_errors_ref.current.delete(vpn_id);
    sign_in_failed_ids.current.delete(vpn_id);
    setSigningInIds(new Set(signing_in_ref.current));
    setActionErrors(new Map(action_errors_ref.current));
    try {
      // The native command reloads the runtime and validates its URL before opening the browser.
      await openVpnSignIn(vpn_id);
    } catch (failure) {
      if (mounted.current && started_lifetime === lifetime.current && runtime_generations.current.get(vpn_id) === generation) {
        sign_in_failed_ids.current.add(vpn_id);
        action_errors_ref.current.set(vpn_id, errorMessage(failure));
        setActionErrors(new Map(action_errors_ref.current));
      }
    } finally {
      signing_in_ref.current.delete(vpn_id);
      if (mounted.current && started_lifetime === lifetime.current) setSigningInIds(new Set(signing_in_ref.current));
    }
  }, []);

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
    statuses, supports_multiple, supported_providers, signing_in_ids, status_loaded, status_loading, status_stale, status_error, last_checked_at,
    actions, action_errors, uncertain_ids, profile_busy, deleting_id, editor, editor_error, editor_saving,
    refresh, connect, stop, signIn, addConnection, editConnection, closeEditor, saveConnection, deleteConnection,
  };
}
