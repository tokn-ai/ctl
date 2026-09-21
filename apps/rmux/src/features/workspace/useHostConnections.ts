import { useCallback, useEffect, useRef, useState } from "react";
import { disconnectSshHost, sshConnectionStatus } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type {
  ConnectionTarget,
  HostConnectionChange,
  HostConnectionStatus,
  SshConnectionTarget,
  WorkspaceHost,
  WorkspaceSshGateway,
} from "../../lib/types";
import { hostTarget } from "./workspaceModel";
import { sameSshEndpoint } from "./remoteRecovery";

interface Options {
  ready: boolean;
  closing: boolean;
  hosts: readonly WorkspaceHost[];
  targets: readonly ConnectionTarget[];
  gateways: readonly WorkspaceSshGateway[];
  /** Quiesce local views and forwards before closing the shared SSH masters. */
  onPause(host_id: string): Promise<void>;
  onResume(host_id: string): void;
}

const EMPTY: HostConnectionStatus = { state: "checking", method_names: [], message: null };
export const HOST_STATUS_INTERVAL_MS = 5_000;

/** Observe every saved method, including a previously selected runtime route. */
export function hostConnectionTargets(host: WorkspaceHost, options: Pick<Options, "targets" | "gateways">) {
  const methods = host.connection_methods.map((method) => ({
    target: hostTarget(host, options.gateways, method.method_id),
    name: method.name,
  }));
  for (const target of options.targets) {
    if (target.kind !== "ssh" || target.host_id !== host.host_id ||
      methods.some((method) => sameSshEndpoint(method.target, target))) continue;
    methods.push({ target, name: "Previous connection" });
  }
  return methods.filter((method): method is { target: SshConnectionTarget; name: string } => method.target.kind === "ssh");
}

export function useHostConnections(options: Options) {
  const current = useRef(options);
  current.current = options;
  const mounted = useRef(false);
  const statusesRef = useRef(new Map<string, HostConnectionStatus>());
  const [statuses, setStatuses] = useState<ReadonlyMap<string, HostConnectionStatus>>(new Map());
  const paused = useRef(new Set<string>());
  const disconnectErrors = useRef(new Map<string, string>());
  const generations = useRef(new Map<string, number>());
  const polling = useRef(false);
  const refreshQueued = useRef(false);
  const configurationGeneration = useRef(0);

  const publish = useCallback((host_id: string, status: HostConnectionStatus) => {
    if (!mounted.current) return;
    statusesRef.current = new Map(statusesRef.current).set(host_id, status);
    setStatuses(statusesRef.current);
  }, []);
  const invalidate = useCallback((host_id: string) => {
    const next = (generations.current.get(host_id) ?? 0) + 1;
    generations.current.set(host_id, next);
    return next;
  }, []);
  const isPaused = useCallback((target: ConnectionTarget) =>
    target.kind === "ssh" && paused.current.has(target.host_id!), []);

  const refresh = useCallback(async (): Promise<void> => {
    const snapshot = current.current;
    if (!snapshot.ready || snapshot.closing || !mounted.current) return;
    if (polling.current) { refreshQueued.current = true; return; }
    polling.current = true;
    const configuration = configurationGeneration.current;
    try {
      await Promise.all(snapshot.hosts.filter((host) => host.host_id !== "local").map(async (host) => {
        const host_id = host.host_id;
        const generation = generations.current.get(host_id) ?? 0;
        const isCurrent = () => mounted.current && !current.current.closing &&
          configuration === configurationGeneration.current &&
          (generations.current.get(host_id) ?? 0) === generation &&
          current.current.hosts.some((item) => item.host_id === host_id);
        if (["connecting", "disconnecting"].includes(statusesRef.current.get(host_id)?.state ?? "")) return;
        let methods: ReturnType<typeof hostConnectionTargets>;
        try { methods = hostConnectionTargets(host, snapshot); }
        catch (failure) {
          publish(host_id, { ...EMPTY, state: "error", message: errorMessage(failure) });
          return;
        }
        const observations = await Promise.all(methods.map(async (method) => {
          try { return { ...method, status: await sshConnectionStatus(method.target), error: null }; }
          catch (failure) { return { ...method, status: null, error: errorMessage(failure) }; }
        }));
        if (!isCurrent()) return;
        const connected = observations.filter((item) => item.status?.connected);
        const usableConnection = connected.some((item) => !item.status?.manually_disconnected);
        const manuallyDisconnected = !usableConnection && observations.some((item) => item.status?.manually_disconnected);
        const failure = observations.find((item) => item.error)?.error;
        if (manuallyDisconnected && !paused.current.has(host_id)) {
          paused.current.add(host_id);
          try { await current.current.onPause(host_id); }
          catch (failure) {
            if (isCurrent()) publish(host_id, { ...EMPTY, state: "error", message: errorMessage(failure) });
            return;
          }
          if (!isCurrent()) return;
        }
        const disconnectFailure = disconnectErrors.current.get(host_id);
        if (usableConnection && !disconnectFailure && paused.current.delete(host_id)) {
          current.current.onResume(host_id);
        }
        const previous = statusesRef.current.get(host_id);
        const incompleteDisconnect = manuallyDisconnected && connected.length > 0;
        const message = disconnectFailure ?? failure ?? (incompleteDisconnect ? "The SSH connection is still open after a disconnect. Retry Disconnect host or connect again."
          : manuallyDisconnected ? "Disconnected manually. Connect this host to resume."
          : !connected.length && previous?.state === "error" ? previous.message : null);
        publish(host_id, {
          state: disconnectFailure || incompleteDisconnect ? "error" : connected.length ? "connected" : message && !manuallyDisconnected ? "error" : "disconnected",
          method_names: disconnectFailure ? previous?.method_names ?? [] : connected.map((method) => method.name),
          message,
        });
      }));
    } finally {
      polling.current = false;
      if (refreshQueued.current) {
        refreshQueued.current = false;
        void refresh();
      }
    }
  }, [publish]);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  // Settings edits invalidate observations of the old route. Polling never
  // authenticates or adds unused SSH config projections to the sidebar.
  const configuration = JSON.stringify([options.hosts, options.targets, options.gateways]);
  useEffect(() => {
    configurationGeneration.current += 1;
    void refresh();
    const timer = setInterval(() => { void refresh(); }, HOST_STATUS_INTERVAL_MS);
    const onFocus = () => { void refresh(); };
    window.addEventListener("focus", onFocus);
    return () => { clearInterval(timer); window.removeEventListener("focus", onFocus); };
  }, [options.ready, options.closing, configuration, refresh]);

  const connectionChanged = useCallback<HostConnectionChange>((target, state, message) => {
    if (target.kind !== "ssh" || !target.host_id) return;
    const host_id = target.host_id;
    if (statusesRef.current.get(host_id)?.state === "disconnecting") return;
    invalidate(host_id);
    const previous = statusesRef.current.get(host_id) ?? EMPTY;
    if (state === "connecting") {
      publish(host_id, { ...previous, state: "connecting", message: null });
    } else if (state === "error") {
      publish(host_id, { ...previous, state: "error", message: message ?? "Connection failed." });
    } else {
      if (state === "connected") {
        disconnectErrors.current.delete(host_id);
        if (paused.current.delete(host_id)) current.current.onResume(host_id);
      }
      publish(host_id, { ...previous, state: "checking", message: null });
      void refresh();
    }
  }, [invalidate, publish, refresh]);

  const disconnect = useCallback(async (target: ConnectionTarget) => {
    if (target.kind !== "ssh" || !target.host_id || current.current.closing) return;
    const host_id = target.host_id;
    if (statusesRef.current.get(host_id)?.state === "disconnecting") return;
    const host = current.current.hosts.find((item) => item.host_id === host_id);
    if (!host) throw new Error("This host is no longer available.");
    const targets = hostConnectionTargets(host, current.current).map((method) => method.target);
    const generation = invalidate(host_id);
    paused.current.add(host_id);
    disconnectErrors.current.delete(host_id);
    publish(host_id, { ...(statusesRef.current.get(host_id) ?? EMPTY), state: "disconnecting", message: null });
    try {
      await current.current.onPause(host_id);
      await disconnectSshHost(targets);
      if (generations.current.get(host_id) === generation) {
        publish(host_id, { state: "disconnected", method_names: [], message: "Disconnected manually. Connect this host to resume." });
      }
    } catch (failure) {
      disconnectErrors.current.set(host_id, errorMessage(failure));
      publish(host_id, { ...(statusesRef.current.get(host_id) ?? EMPTY), state: "error", message: errorMessage(failure) });
      throw failure;
    }
  }, [invalidate, publish]);

  return { statuses, refresh, isPaused, connectionChanged, disconnect };
}
