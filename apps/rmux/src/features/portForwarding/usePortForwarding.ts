import { useCallback, useEffect, useRef, useState } from "react";
import { configurePortForward, listPortForwards } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type {
  ConnectionTarget,
  PortForwardStatus,
  SshConnectionTarget,
  WorkspacePortForward,
} from "../../lib/types";

type UpdateForwards = (
  update: (current: WorkspacePortForward[]) => WorkspacePortForward[],
) => void;

export interface PortForwardingController {
  statuses: ReadonlyMap<string, PortForwardStatus>;
  busy: ReadonlySet<string>;
  hostErrors: ReadonlyMap<string, string>;
  refreshing: boolean;
  lastRefreshedAt: number | null;
  refreshAll(): Promise<void>;
  refreshTarget(target: SshConnectionTarget): Promise<void>;
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
  targetsRef.current = targets;
  forwardsRef.current = forwards;

  const refreshTarget = useCallback(async (target: SshConnectionTarget) => {
    const hostId = target.host_id!;
    setHostErrors((current) => withoutKey(current, hostId));
    try {
      for (const forward of forwardsRef.current) {
        if (forward.host_id === hostId && forward.enabled) {
          await configurePortForward(target, forward, true);
        }
      }
      const next = await listPortForwards(target);
      const hostForwardIds = new Set(
        forwardsRef.current
          .filter((forward) => forward.host_id === hostId)
          .map((forward) => forward.forward_id),
      );
      setStatuses((current) => {
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
      setHostErrors((current) => new Map(current).set(hostId, errorMessage(failure)));
      const hostForwardIds = new Set(
        forwardsRef.current
          .filter((forward) => forward.host_id === hostId)
          .map((forward) => forward.forward_id),
      );
      setStatuses(
        (current) =>
          new Map(
            [...current].filter(
              ([forwardId]) => !hostForwardIds.has(forwardId),
            ),
          ),
      );
    }
  }, []);

  const refreshAll = useCallback(async () => {
    setRefreshing(true);
    const hostIds = new Set(
      forwardsRef.current.map((forward) => forward.host_id),
    );
    const sshTargets = targetsRef.current.filter(
      (target): target is SshConnectionTarget =>
        target.kind === "ssh" && hostIds.has(target.host_id!),
    );
    try {
      await Promise.all(sshTargets.map(refreshTarget));
      setLastRefreshedAt(Date.now());
    } finally {
      setRefreshing(false);
    }
  }, [refreshTarget]);

  const setEnabled = useCallback(
    async (
      target: SshConnectionTarget,
      forward: WorkspacePortForward,
      enabled: boolean,
    ) => {
      const hostId = target.host_id!;
      setBusy((current) => new Set(current).add(forward.forward_id));
      setHostErrors((current) => withoutKey(current, hostId));
      try {
        const status = await configurePortForward(target, forward, enabled);
        setStatuses((current) => {
          const next = new Map(current);
          if (enabled) next.set(forward.forward_id, status);
          else next.delete(forward.forward_id);
          return next;
        });
        updateForwards((current) =>
          current.map((item) =>
            item.forward_id === forward.forward_id
              ? { ...item, enabled }
              : item,
          ),
        );
      } catch (failure) {
        const message = errorMessage(failure);
        setHostErrors((current) => new Map(current).set(hostId, message));
        throw failure;
      } finally {
        setBusy((current) => {
          const next = new Set(current);
          next.delete(forward.forward_id);
          return next;
        });
      }
    },
    [updateForwards],
  );

  useEffect(() => {
    if (!ready || restoredRef.current) return;
    restoredRef.current = true;
    void refreshAll();
  }, [ready, refreshAll]);

  useEffect(() => {
    const valid = new Set(forwards.map((forward) => forward.forward_id));
    setStatuses((current) => {
      if ([...current.keys()].every((forwardId) => valid.has(forwardId))) {
        return current;
      }
      return new Map([...current].filter(([forwardId]) => valid.has(forwardId)));
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
    setEnabled,
  };
}

function withoutKey<K, V>(map: ReadonlyMap<K, V>, key: K): Map<K, V> {
  const next = new Map(map);
  next.delete(key);
  return next;
}
