import { useCallback, useEffect, useRef, useState } from "react";
import { sshReachability } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type {
  HostConnectionStatus,
  HostReachabilityObservation,
  SshConnectionTarget,
  SshReachability,
  VpnStatus,
  WorkspaceHost,
  WorkspaceSshGateway,
} from "../../lib/types";
import { hostTarget } from "./workspaceModel";
import { localVpnConnectionId } from "./sshRoute";

export const HOST_REACHABILITY_INTERVAL_MS = 30_000;
export const HOST_REACHABILITY_TIMEOUT_MS = 8_000;
const MAX_CONCURRENT_PROBES = 4;

interface Options {
  ready: boolean;
  closing: boolean;
  hosts: readonly WorkspaceHost[];
  gateways: readonly WorkspaceSshGateway[];
  statuses: ReadonlyMap<string, HostConnectionStatus>;
  vpn_statuses?: readonly VpnStatus[];
  vpn_status_stale?: boolean;
}

interface ProbeMethod {
  name: string;
  target: SshConnectionTarget | null;
  error: string | null;
}

interface ProbeConfiguration {
  key: string;
  generation: number;
  eligible: boolean;
  methods: ProbeMethod[];
}

const checkingReachability = (): HostReachabilityObservation => ({
  state: "checking", reason: null, method_names: [], message: null, checked_at_ms: null,
});

function aggregateReachability(results: readonly { name: string; result: SshReachability }[]): HostReachabilityObservation {
  const available = results.filter(({ result }) => result.state === "available");
  const unknown = results.find(({ result }) => result.state === "unknown");
  const skipped = results.find(({ result }) => result.reason === "vpn_disconnected") ??
    results.find(({ result }) => result.state === "not_checked");
  const selected = available[0] ?? unknown ?? skipped ?? results[0];
  return {
    state: selected?.result.state ?? "not_checked",
    reason: selected?.result.reason ?? null,
    method_names: available.map(({ name }) => name),
    message: results.filter(({ result }) => result.message).map(({ name, result }) => `${name}: ${result.message}`).join("\n") || null,
    checked_at_ms: Date.now(),
  };
}

async function checkSshGreeting(target: SshConnectionTarget): Promise<SshReachability> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const deadline = new Promise<SshReachability>((resolve) => {
      timer = setTimeout(() => resolve({ state: "unknown", reason: "check_failed", message: "SSH reachability query timed out." }), HOST_REACHABILITY_TIMEOUT_MS);
    });
    return await Promise.race([sshReachability(target), deadline]);
  } catch (failure) {
    return { state: "unknown", reason: "check_failed", message: errorMessage(failure) };
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** Reachability never changes master evidence, manual pause, or recovery policy. */
export function useHostReachability(options: Options) {
  const mounted = useRef(false);
  const sequence = useRef(0);
  const configurations = useRef(new Map<string, ProbeConfiguration>());
  const observations = useRef(new Map<string, { generation: number; observation: HostReachabilityObservation }>());
  const [, render] = useState(0);
  const inFlight = useRef(new Map<number, Promise<void>>());
  const active = useRef(0);
  const waiting = useRef<Array<() => void>>([]);

  const next = new Map<string, ProbeConfiguration>();
  for (const host of options.hosts) {
    if (host.host_id === "local") continue;
    // Historical runtime routes remain relevant to master ownership, but only
    // current saved/projected methods may advertise a new connection as available.
    const methods = host.connection_methods.map((method): ProbeMethod => {
      try {
        const target = hostTarget(host, options.gateways, method.method_id, options.hosts);
        return target.kind === "ssh" && !target.unavailable
          ? { name: method.name, target, error: null }
          : { name: method.name, target: null, error: target.kind === "ssh" ? target.unavailable ?? null : null };
      } catch (failure) {
        return { name: method.name, target: null, error: errorMessage(failure) };
      }
    });
    const vpnIds = new Set(methods.flatMap(({ target }) => {
      const vpn_id = target ? localVpnConnectionId(target) : undefined;
      return vpn_id ? [vpn_id] : [];
    }));
    // Poll timestamps and unrelated VPNs must not invalidate a valid result.
    const vpnRoutes = (options.vpn_statuses ?? []).filter((status) => status.connection_id && vpnIds.has(status.connection_id))
      .map(({ connection_id, vpn_id, state, running, endpoint, container_name }) => ({ connection_id, vpn_id, state, running, endpoint, container_name }))
      .sort((left, right) => (left.connection_id ?? "").localeCompare(right.connection_id ?? ""));
    const status = options.statuses.get(host.host_id);
    const connected = status?.observation ? status.observation.availability === "available" : status?.state === "connected";
    const pending = !status || status.observation?.completeness === "pending" || status.operation?.state === "pending";
    const eligible = options.ready && !options.closing && !connected && !pending;
    const key = JSON.stringify([methods, vpnRoutes, vpnIds.size ? options.vpn_status_stale : false, eligible]);
    const previous = configurations.current.get(host.host_id);
    next.set(host.host_id, { key, eligible, methods,
      generation: previous?.key === key ? previous.generation : ++sequence.current });
  }
  configurations.current = next;
  const configurationKey = JSON.stringify([...next].map(([id, configuration]) => [id, configuration.generation]));

  const refresh = useCallback(async (force = false) => {
    if (!mounted.current) return;
    const checks: Promise<void>[] = [];
    for (const [host_id, configuration] of configurations.current) {
      if (!configuration.eligible) continue;
      const existing = inFlight.current.get(configuration.generation);
      if (existing) { checks.push(existing); continue; }
      const observed = observations.current.get(host_id);
      if (!force && observed?.generation === configuration.generation && observed.observation.checked_at_ms !== null &&
        Date.now() - observed.observation.checked_at_ms < HOST_REACHABILITY_INTERVAL_MS) continue;
      const isCurrent = () => mounted.current && configurations.current.get(host_id)?.generation === configuration.generation;
      const pending = (async () => {
        const results = await Promise.all(configuration.methods.map(async (method) => {
          if (!method.target) return { name: method.name, result: {
            state: "not_checked", reason: "unsupported_configuration", message: method.error,
          } as SshReachability };
          // Transfer the occupied slot directly to the next waiter so a newly
          // enqueued request cannot overtake it and exceed the concurrency bound.
          if (active.current < MAX_CONCURRENT_PROBES) active.current += 1;
          else await new Promise<void>((resolve) => waiting.current.push(resolve));
          try {
            return { name: method.name, result: isCurrent() ? await checkSshGreeting(method.target) : {
              state: "not_checked", reason: null, message: null,
            } as SshReachability };
          } finally {
            const resume = waiting.current.shift();
            if (resume) resume();
            else active.current -= 1;
          }
        }));
        if (isCurrent()) {
          observations.current.set(host_id, { generation: configuration.generation, observation: aggregateReachability(results) });
          render((revision) => revision + 1);
        }
      })().finally(() => { inFlight.current.delete(configuration.generation); });
      inFlight.current.set(configuration.generation, pending);
      checks.push(pending);
    }
    await Promise.all(checks);
  }, []);

  useEffect(() => {
    mounted.current = true;
    const timer = setInterval(() => { void refresh(); }, 5_000);
    const onFocus = () => { void refresh(true); };
    window.addEventListener("focus", onFocus);
    return () => { mounted.current = false; clearInterval(timer); window.removeEventListener("focus", onFocus); };
  }, [refresh]);

  useEffect(() => {
    for (const host_id of observations.current.keys()) if (!configurations.current.has(host_id)) observations.current.delete(host_id);
    void refresh();
  }, [configurationKey, refresh]);

  const statuses = new Map([...next].map(([host_id, configuration]) => {
    const observed = observations.current.get(host_id);
    return [host_id, observed?.generation === configuration.generation ? observed.observation : checkingReachability()];
  }));
  return { statuses, refresh };
}
