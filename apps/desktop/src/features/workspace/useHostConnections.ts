import { useCallback, useEffect, useRef, useState } from "react";
import { disconnectSshHost, sshConnectionStatus } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type {
  ConnectionTarget,
  HostConnectionChange,
  HostConnectionStatus,
  SshConnectionStatus,
  SshConnectionTarget,
  WorkspaceHost,
  WorkspaceSshGateway,
  VpnStatus,
} from "../../lib/types";
import { useHostReachability } from "./useHostReachability";
import { hostTarget } from "./workspaceModel";
import { sameSshEndpoint } from "./remoteRecovery";
import {
  aggregateHostObservations,
  beginHostOperation,
  emptyHostConnection,
  failHostOperation,
  finishHostConnection,
  finishHostDisconnect,
  hostConnectionStatus,
  observeHostConnection,
  type HostConnectionModel,
} from "./hostConnectionState";

interface Options {
  ready: boolean;
  closing: boolean;
  hosts: readonly WorkspaceHost[];
  targets: readonly ConnectionTarget[];
  gateways: readonly WorkspaceSshGateway[];
  vpn_statuses?: readonly VpnStatus[];
  vpn_status_stale?: boolean;
  /** Quiesce local views and forwards before releasing the host's connection. */
  onPause(host_id: string): Promise<void>;
  onResume(host_id: string): void;
}

export const HOST_STATUS_INTERVAL_MS = 5_000;
// Native checks are bounded too; this also covers a stalled broker IPC request.
export const HOST_STATUS_TIMEOUT_MS = 5_000;

async function observeSshMaster(target: SshConnectionTarget): Promise<SshConnectionStatus> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const deadline = new Promise<never>((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error("SSH status query timed out.")), HOST_STATUS_TIMEOUT_MS);
    });
    // Late native results are consumed by this settled race and cannot publish.
    return await Promise.race([sshConnectionStatus(target), deadline]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** Observe every saved method, including a previously selected runtime route. */
export function hostConnectionTargets(host: WorkspaceHost, options: Pick<Options, "targets" | "gateways">) {
  return hostConnectionCandidates(host, options).map((method) => {
    if (!method.target) throw new Error(method.error ?? "This SSH method could not be resolved.");
    return { target: method.target, name: method.name };
  });
}

function hostConnectionCandidates(host: WorkspaceHost, options: Pick<Options, "targets" | "gateways">) {
  const methods: { target: SshConnectionTarget | null; name: string; error: string | null }[] = [];
  for (const method of host.connection_methods) {
    try {
      const target = hostTarget(host, options.gateways, method.method_id);
      if (target.kind === "ssh") methods.push({ target, name: method.name, error: null });
    } catch (failure) {
      methods.push({ target: null, name: method.name, error: errorMessage(failure) });
    }
  }
  for (const target of options.targets) {
    if (target.kind !== "ssh" || target.host_id !== host.host_id ||
      methods.some((method) => method.target && sameSshEndpoint(method.target, target))) continue;
    methods.push({ target, name: "Previous connection", error: null });
  }
  return methods;
}

export function useHostConnections(options: Options) {
  const current = useRef(options);
  current.current = options;
  const mounted = useRef(false);
  const models = useRef(new Map<string, HostConnectionModel>());
  const [published, setPublished] = useState({ generation: -1, statuses: new Map<string, HostConnectionStatus>() });
  const modelsGeneration = useRef(-1);
  const paused = useRef(new Set<string>());
  const generations = useRef(new Map<string, number>());
  const attempts = useRef(new Map<string, { target: SshConnectionTarget; configuration: number }>());
  const polling = useRef(false);
  const refreshQueued = useRef(false);
  const configurationKey = JSON.stringify([options.ready, options.closing, options.hosts, options.targets, options.gateways]);
  const configuration = useRef({ key: configurationKey, generation: 0 });
  // Reject old promises as soon as a render sees new settings, including the
  // interval before its effect runs and a change followed by a change back.
  if (configuration.current.key !== configurationKey) {
    configuration.current = { key: configurationKey, generation: configuration.current.generation + 1 };
  }

  const model = useCallback((host_id: string) => modelsGeneration.current === configuration.current.generation
    ? models.current.get(host_id) ?? emptyHostConnection()
    : emptyHostConnection(true), []);
  const publish = useCallback((host_id: string, next: HostConnectionModel) => {
    if (!mounted.current) return;
    if (modelsGeneration.current !== configuration.current.generation) models.current = new Map();
    modelsGeneration.current = configuration.current.generation;
    models.current.set(host_id, next);
    setPublished({ generation: configuration.current.generation,
      statuses: new Map([...models.current].map(([id, item]) => [id, hostConnectionStatus(item)])) });
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
    const observedConfiguration = configuration.current.generation;
    try {
      await Promise.all(snapshot.hosts.filter((host) => host.host_id !== "local").map(async (host) => {
        const host_id = host.host_id;
        const generation = generations.current.get(host_id) ?? 0;
        const isCurrent = () => mounted.current && current.current.ready && !current.current.closing &&
          observedConfiguration === configuration.current.generation &&
          (generations.current.get(host_id) ?? 0) === generation &&
          current.current.hosts.some((item) => item.host_id === host_id);
        const operation = model(host_id).operation;
        if (operation.kind === "disconnect" && operation.state === "pending") return;
        const methods = hostConnectionCandidates(host, snapshot);
        const observations = await Promise.all(methods.map(async (method) => {
          if (!method.target) return { name: method.name, status: null, error: method.error };
          try { return { name: method.name, status: await observeSshMaster(method.target), error: null }; }
          catch (failure) { return { name: method.name, status: null, error: errorMessage(failure) }; }
        }));
        if (!isCurrent()) return;
        const result = aggregateHostObservations(observations, Date.now());
        if (result.manually_disconnected && !paused.current.has(host_id)) {
          paused.current.add(host_id);
          // Evidence is already known even if quiescing the views takes time.
          publish(host_id, observeHostConnection(model(host_id), result));
          try { await current.current.onPause(host_id); }
          catch (failure) {
            if (isCurrent()) publish(host_id, failHostOperation(observeHostConnection(model(host_id), result), "disconnect", errorMessage(failure)));
            return;
          }
          if (!isCurrent()) return;
        }
        const previous = model(host_id);
        const disconnectFailed = previous.operation.kind === "disconnect" && previous.operation.state === "failed";
        if (result.usable_connection && !disconnectFailed && previous.operation.state !== "pending" && paused.current.delete(host_id)) {
          current.current.onResume(host_id);
        }
        if (isCurrent()) publish(host_id, observeHostConnection(previous, result));
      }));
    } finally {
      polling.current = false;
      if (refreshQueued.current) {
        refreshQueued.current = false;
        void refresh();
      }
    }
  }, [model, publish]);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  // Invalidate published evidence immediately, not only the old request. An
  // unresolved poll for edited settings must not leave a connected badge behind.
  useEffect(() => {
    const hostIds = new Set(current.current.hosts.map((host) => host.host_id));
    for (const id of paused.current) if (!hostIds.has(id)) paused.current.delete(id);
    for (const id of generations.current.keys()) if (!hostIds.has(id)) generations.current.delete(id);
    for (const id of attempts.current.keys()) if (!hostIds.has(id)) attempts.current.delete(id);
    const previous = models.current;
    models.current = new Map(current.current.hosts.filter((host) => host.host_id !== "local").map((host) => [host.host_id, emptyHostConnection(previous.has(host.host_id))]));
    modelsGeneration.current = configuration.current.generation;
    setPublished({ generation: configuration.current.generation,
      statuses: new Map([...models.current].map(([id, item]) => [id, hostConnectionStatus(item)])) });
    void refresh();
    const timer = setInterval(() => { void refresh(); }, HOST_STATUS_INTERVAL_MS);
    const onFocus = () => { void refresh(); };
    window.addEventListener("focus", onFocus);
    return () => { clearInterval(timer); window.removeEventListener("focus", onFocus); };
  }, [configurationKey, refresh]);

  const connectionChanged = useCallback<HostConnectionChange>((target, state, message) => {
    if (target.kind !== "ssh" || !target.host_id) return;
    const host_id = target.host_id;
    const host = current.current.hosts.find((item) => item.host_id === host_id);
    if (!host || !current.current.ready || current.current.closing || !mounted.current) return;
    const previous = model(host_id);
    if (previous.operation.kind === "disconnect" && previous.operation.state === "pending") return;
    if (state !== "connecting") {
      const attempt = attempts.current.get(host_id);
      if (attempt && (attempt.configuration !== configuration.current.generation || !sameSshEndpoint(attempt.target, target))) return;
    }
    invalidate(host_id);
    if (state === "connecting") {
      attempts.current.set(host_id, { target, configuration: configuration.current.generation });
      const name = host.connection_methods.find((method) => method.method_id === target.method_id)?.name ?? null;
      publish(host_id, beginHostOperation(previous, "connect", name));
    } else if (state === "error") {
      publish(host_id, failHostOperation(previous, "connect", message ?? "Connection failed."));
    } else {
      attempts.current.delete(host_id);
      if (state === "connected" && paused.current.delete(host_id)) current.current.onResume(host_id);
      publish(host_id, finishHostConnection(previous, state === "connected"));
      void refresh();
    }
  }, [invalidate, model, publish, refresh]);

  const disconnect = useCallback(async (target: ConnectionTarget) => {
    if (target.kind !== "ssh" || !target.host_id || !current.current.ready || current.current.closing) return;
    const host_id = target.host_id;
    const previous = model(host_id);
    if (previous.operation.kind === "disconnect" && previous.operation.state === "pending") return;
    const host = current.current.hosts.find((item) => item.host_id === host_id);
    if (!host) throw new Error("This host is no longer available.");
    const targets = hostConnectionTargets(host, current.current).map((method) => method.target);
    const generation = invalidate(host_id);
    const selectedConfiguration = configuration.current.generation;
    const isCurrent = () => mounted.current && !current.current.closing &&
      selectedConfiguration === configuration.current.generation &&
      generations.current.get(host_id) === generation &&
      current.current.hosts.some((item) => item.host_id === host_id);
    paused.current.add(host_id);
    attempts.current.delete(host_id);
    publish(host_id, beginHostOperation(previous, "disconnect", null));
    let requested = false;
    try {
      await current.current.onPause(host_id);
      if (!isCurrent()) return;
      requested = true;
      await disconnectSshHost(targets);
      if (isCurrent()) publish(host_id, finishHostDisconnect(Date.now()));
    } catch (failure) {
      if (isCurrent()) {
        // A failed disconnect can have partially changed the masters. Preserve
        // the error, but do not present pre-disconnect evidence as current.
        const evidence = requested ? emptyHostConnection(true) : model(host_id);
        publish(host_id, failHostOperation(evidence, "disconnect", errorMessage(failure)));
      }
      throw failure;
    }
  }, [invalidate, model, publish]);

  // Mask the old configuration during render as well as in the effect, so it
  // cannot briefly paint an available badge for newly edited settings.
  const statuses: ReadonlyMap<string, HostConnectionStatus> = published.generation === configuration.current.generation
    ? published.statuses
    : new Map(options.hosts.filter((host) => host.host_id !== "local").map((host) => [host.host_id, hostConnectionStatus(emptyHostConnection(models.current.has(host.host_id)))]));
  const reachability = useHostReachability({ ...options, statuses });
  const observedStatuses = new Map([...statuses].map(([host_id, status]) =>
    [host_id, { ...status, reachability: reachability.statuses.get(host_id) }]));
  const refreshReachability = reachability.refresh;
  const refreshAll = useCallback(async () => {
    await Promise.all([refresh(), refreshReachability(true)]);
  }, [refresh, refreshReachability]);
  return { statuses: observedStatuses, refresh: refreshAll, isPaused, connectionChanged, disconnect };
}
