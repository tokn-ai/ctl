import { useCallback, useEffect, useRef, useState } from "react";
import { configurePortForward, listPortForwards } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import { sameSshEndpoint } from "../workspace/remoteRecovery";
import type {
  ConnectionTarget,
  PortForwardStatus,
  SshConnectionTarget,
  WorkspacePortForward,
} from "../../lib/types";

type UpdateForwards = (
  update: (current: WorkspacePortForward[]) => WorkspacePortForward[],
) => void;

interface HostOperation {
  generation: number;
  target: SshConnectionTarget;
  pending: Promise<void>;
}

export interface PortForwardingController {
  statuses: ReadonlyMap<string, PortForwardStatus>;
  busy: ReadonlySet<string>;
  hostErrors: ReadonlyMap<string, string>;
  refreshing: boolean;
  lastRefreshedAt: number | null;
  refreshAll(): Promise<void>;
  refreshTarget(target: SshConnectionTarget): Promise<void>;
  pauseHost(host_id: string): Promise<void>;
  resumeHost(host_id: string): void;
  setEnabled(
    target: SshConnectionTarget,
    forward: WorkspacePortForward,
    enabled: boolean,
  ): Promise<void>;
}

export function usePortForwarding(
  ready: boolean,
  targets: readonly ConnectionTarget[],
  forwards: readonly WorkspacePortForward[],
  updateForwards: UpdateForwards,
): PortForwardingController {
  const [statuses, setStatuses] = useState<ReadonlyMap<string, PortForwardStatus>>(
    new Map(),
  );
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set());
  const [hostErrors, setHostErrors] = useState<ReadonlyMap<string, string>>(
    new Map(),
  );
  const [refreshing, setRefreshing] = useState(false);
  const [lastRefreshedAt, setLastRefreshedAt] = useState<number | null>(null);
  const targetsRef = useRef(targets);
  const forwardsRef = useRef(forwards);
  const restoredRef = useRef(false);
  const operationsRef = useRef(new Map<string, HostOperation>());
  const pausedHostsRef = useRef(new Set<string>());
  const busyCountsRef = useRef(new Map<string, number>());
  const refreshCountRef = useRef(0);
  const refreshGenerationRef = useRef(0);
  const runtimeTargetsRef = useRef(new Map<string, SshConnectionTarget>());
  const runtimeTargets = new Map(targets.flatMap((target) => target.kind === "ssh"
    ? [[target.host_id!, target] as const]
    : []));
  for (const [hostId, previous] of runtimeTargetsRef.current) {
    const next = runtimeTargets.get(hostId);
    const operation = operationsRef.current.get(hostId);
    if (operation && (!next || !sameSshEndpoint(previous, next)) &&
      (!next || !sameSshEndpoint(operation.target, next))) {
      operation.generation += 1;
      if (next) operation.target = next;
    }
  }
  runtimeTargetsRef.current = runtimeTargets;
  targetsRef.current = targets;
  forwardsRef.current = forwards;

  // Dialogs and background refreshes may hold an older render's target. Only
  // refreshTarget (used after an explicit connection) selects a new method.
  const selectedTarget = useCallback((target: SshConnectionTarget) => {
    const current = runtimeTargetsRef.current.get(target.host_id!);
    if (!current) throw new Error("This host is no longer in the workspace.");
    return operationsRef.current.get(target.host_id!)?.target ?? current;
  }, []);

  // An older request must finish before a new method can take ownership of the
  // same forward. Invalidation also stops an old batch between awaited calls.
  const runHostOperation = useCallback((
    target: SshConnectionTarget,
    run: (isCurrent: () => boolean) => Promise<void>,
  ) => {
    const hostId = target.host_id!;
    if (!runtimeTargetsRef.current.has(hostId)) return Promise.resolve();
    const operation = operationsRef.current.get(hostId) ?? {
      generation: 0, target, pending: Promise.resolve(),
    };
    operationsRef.current.set(hostId, operation);
    if (!sameSshEndpoint(operation.target, target)) operation.generation += 1;
    operation.target = target;
    const generation = operation.generation;
    const isCurrent = () => operation.generation === generation;
    const pending = operation.pending.then(async () => {
      if (isCurrent()) await run(isCurrent);
    });
    operation.pending = pending.catch(() => undefined);
    return pending;
  }, []);

  const pauseHost = useCallback((host_id: string) => {
    const operation = operationsRef.current.get(host_id);
    if (!pausedHostsRef.current.has(host_id)) {
      pausedHostsRef.current.add(host_id);
      if (operation) operation.generation += 1;
      setHostErrors((current) => withoutKey(current, host_id));
      setStatuses((current) => pausedStatuses(current, forwardsRef.current, pausedHostsRef.current));
    }
    // In-flight enables clean themselves up after invalidation. Disconnecting
    // the master must wait until that cleanup and the old queue have finished.
    return operation?.pending ?? Promise.resolve();
  }, []);

  const resumeHost = useCallback((host_id: string) => {
    pausedHostsRef.current.delete(host_id);
  }, []);

  const refreshTarget = useCallback((target: SshConnectionTarget) => {
    if (pausedHostsRef.current.has(target.host_id!)) return Promise.resolve();
    return runHostOperation(target, async (isCurrent) => {
      const hostId = target.host_id!;
      setHostErrors((current) => isCurrent() ? withoutKey(current, hostId) : current);
      try {
        for (const forward of forwardsRef.current) {
          if (!isCurrent()) return;
          if (forward.host_id === hostId && forward.enabled) {
            await configurePortForward(target, forward, true);
            if (!isCurrent()) {
              await configurePortForward(target, forward, false);
              return;
            }
          }
        }
        if (!isCurrent()) return;
        const next = await listPortForwards(target);
        if (!isCurrent()) return;
        const hostForwardIds = new Set(
          forwardsRef.current
            .filter((forward) => forward.host_id === hostId)
            .map((forward) => forward.forward_id),
        );
        setStatuses((current) => {
          if (!isCurrent()) return current;
          const merged = new Map(current);
          for (const forwardId of hostForwardIds) merged.delete(forwardId);
          for (const status of next) {
            if (hostForwardIds.has(status.forward.forward_id)) {
              merged.set(status.forward.forward_id, status);
            }
          }
          return merged;
        });
      } catch (failure) {
        if (!isCurrent()) return;
        setHostErrors((current) => isCurrent() ? new Map(current).set(hostId, errorMessage(failure)) : current);
        const hostForwardIds = new Set(
          forwardsRef.current
            .filter((forward) => forward.host_id === hostId)
            .map((forward) => forward.forward_id),
        );
        setStatuses(
          (current) => isCurrent()
            ? new Map(
              [...current].filter(
                ([forwardId]) => !hostForwardIds.has(forwardId),
              ),
            ) : current,
        );
      }
    });
  }, [runHostOperation]);

  const refreshAll = useCallback(async () => {
    const generation = ++refreshGenerationRef.current;
    refreshCountRef.current += 1;
    setRefreshing(true);
    const hostIds = new Set(
      forwardsRef.current.map((forward) => forward.host_id),
    );
    const sshTargets = targetsRef.current.filter(
      (target): target is SshConnectionTarget =>
        target.kind === "ssh" && hostIds.has(target.host_id!),
    );
    try {
      await Promise.all(sshTargets.map((target) => refreshTarget(selectedTarget(target))));
      if (refreshGenerationRef.current === generation) setLastRefreshedAt(Date.now());
    } finally {
      refreshCountRef.current -= 1;
      setRefreshing(refreshCountRef.current > 0);
    }
  }, [refreshTarget, selectedTarget]);

  const setEnabled = useCallback(
    async (
      target: SshConnectionTarget,
      forward: WorkspacePortForward,
      enabled: boolean,
    ) => {
      target = selectedTarget(target);
      const hostId = target.host_id!;
      if (enabled && pausedHostsRef.current.has(hostId)) {
        throw new Error("Connect this host before starting forwards.");
      }
      busyCountsRef.current.set(forward.forward_id, (busyCountsRef.current.get(forward.forward_id) ?? 0) + 1);
      setBusy((current) => new Set(current).add(forward.forward_id));
      try {
        await runHostOperation(target, async (isCurrent) => {
          setHostErrors((current) => isCurrent() ? withoutKey(current, hostId) : current);
          try {
            const status = await configurePortForward(target, forward, enabled);
            if (!isCurrent()) {
              if (enabled) await configurePortForward(target, forward, false);
              return;
            }
            setStatuses((current) => {
              if (!isCurrent()) return current;
              const next = new Map(current);
              if (enabled) next.set(forward.forward_id, status);
              else next.delete(forward.forward_id);
              return next;
            });
            updateForwards((current) =>
              isCurrent() ? current.map((item) =>
                item.forward_id === forward.forward_id
                  ? { ...item, enabled }
                  : item,
              ) : current,
            );
          } catch (failure) {
            if (!isCurrent()) return;
            const message = errorMessage(failure);
            setHostErrors((current) => isCurrent() ? new Map(current).set(hostId, message) : current);
            throw failure;
          }
        });
      } finally {
        const remaining = (busyCountsRef.current.get(forward.forward_id) ?? 1) - 1;
        if (remaining) busyCountsRef.current.set(forward.forward_id, remaining);
        else busyCountsRef.current.delete(forward.forward_id);
        setBusy((current) => {
          if (busyCountsRef.current.has(forward.forward_id)) return current;
          const next = new Set(current);
          next.delete(forward.forward_id);
          return next;
        });
      }
    },
    [runHostOperation, selectedTarget, updateForwards],
  );

  useEffect(() => {
    if (!ready || restoredRef.current) return;
    restoredRef.current = true;
    void refreshAll();
  }, [ready, refreshAll]);

  useEffect(() => {
    const valid = new Set(forwards.map((forward) => forward.forward_id));
    setStatuses((current) => {
      const retained = [...current.keys()].every((forwardId) => valid.has(forwardId))
        ? current
        : new Map([...current].filter(([forwardId]) => valid.has(forwardId)));
      return pausedStatuses(retained, forwards, pausedHostsRef.current);
    });
  }, [forwards]);

  useEffect(() => {
    const valid = new Set(
      targets
        .filter(
          (target): target is SshConnectionTarget => target.kind === "ssh",
        )
        .map((target) => target.host_id!),
    );
    setHostErrors((current) => {
      if ([...current.keys()].every((hostId) => valid.has(hostId))) {
        return current;
      }
      return new Map([...current].filter(([hostId]) => valid.has(hostId)));
    });
  }, [targets]);

  return {
    statuses,
    busy,
    hostErrors,
    refreshing,
    lastRefreshedAt,
    refreshAll,
    refreshTarget,
    pauseHost,
    resumeHost,
    setEnabled,
  };
}

function pausedStatuses(
  current: ReadonlyMap<string, PortForwardStatus>,
  forwards: readonly WorkspacePortForward[],
  pausedHosts: ReadonlySet<string>,
): ReadonlyMap<string, PortForwardStatus> {
  let next: Map<string, PortForwardStatus> | undefined;
  const message = "Host disconnected. Connect this host to resume forwarding.";
  for (const forward of forwards) {
    if (!pausedHosts.has(forward.host_id)) continue;
    const status = (next ?? current).get(forward.forward_id);
    if (!forward.enabled) {
      if (status) {
        next ??= new Map(current);
        next.delete(forward.forward_id);
      }
    } else if (status?.state !== "waiting_for_authentication" || status.message !== message ||
      status.forward.bind_address !== forward.bind_address ||
      status.forward.local_port !== forward.local_port ||
      status.forward.remote_host !== forward.remote_host ||
      status.forward.remote_port !== forward.remote_port) {
      next ??= new Map(current);
      next.set(forward.forward_id, {
        forward,
        state: "waiting_for_authentication",
        message,
      });
    }
  }
  return next ?? current;
}

function withoutKey<K, V>(map: ReadonlyMap<K, V>, key: K): Map<K, V> {
  const next = new Map(map);
  next.delete(key);
  return next;
}
