import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { errorCode } from "../../lib/errors";
import { sshConnectionStatus } from "../../lib/tauri";
import type { ConnectionTarget, SshConnectionStatus, SshConnectionTarget } from "../../lib/types";
import { sameSshEndpoint } from "../workspace/remoteRecovery";

export type ManualReconnectRequest = (target: ConnectionTarget, signal: AbortSignal, force?: boolean) => Promise<boolean>;
type ConnectHostHandler = (target: SshConnectionTarget, signal: AbortSignal) => Promise<boolean>;

export const MANUAL_RECONNECT_STATUS_TIMEOUT_MS = 5_000;

interface PendingAuthentication {
  target: SshConnectionTarget;
  controller: AbortController;
  participants: number;
  result: Promise<boolean>;
}

/** Consume late results while allowing the requesting attachment to disappear. */
async function waitForResult<T>(operation: Promise<T>, signal: AbortSignal, timeout_ms?: number): Promise<T | null> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let onAbort!: () => void;
  try {
    return await Promise.race([
      operation,
      new Promise<null>((resolve) => {
        onAbort = () => resolve(null);
        signal.addEventListener("abort", onAbort, { once: true });
        if (signal.aborted) onAbort();
      }),
      ...(timeout_ms === undefined ? [] : [new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject({
          code: "ssh_status_timeout",
          message: "SSH connection status could not be checked in time. Retry reconnect.",
        }), timeout_ms);
      })]),
    ]);
  } finally {
    signal.removeEventListener("abort", onAbort);
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** One window's explicit retries share authentication only for the same route. */
export class ManualReconnectCoordinator {
  private handler: ConnectHostHandler | null = null;
  private pending: PendingAuthentication | null = null;

  constructor(private readonly status = sshConnectionStatus) {}

  register(handler: ConnectHostHandler): () => void {
    this.handler = handler;
    return () => {
      if (this.handler !== handler) return;
      this.handler = null;
      this.pending?.controller.abort();
    };
  }

  request: ManualReconnectRequest = async (target, signal, force = false) => {
    if (signal.aborted) return false;
    if (target.kind === "local") return true;
    if (!force) {
      let status: SshConnectionStatus | null;
      try {
        status = await waitForResult(Promise.resolve().then(() => this.status(target)), signal, MANUAL_RECONNECT_STATUS_TIMEOUT_MS);
      } catch (failure) {
        if (signal.aborted) return false;
        if (errorCode(failure) === "ssh_broker_unsupported") return true;
        throw failure;
      }
      if (signal.aborted || status === null) return false;
      if (typeof status?.connected !== "boolean" || typeof status.manually_disconnected !== "boolean") {
        throw { code: "invalid_ssh_status", message: "SSH connection status could not be checked. Retry reconnect." };
      }
      if (status.connected && !status.manually_disconnected) return true;
    }
    if (signal.aborted) return false;
    let pending = this.pending;
    if (pending && (!sameSshEndpoint(pending.target, target) || pending.target.host_id !== target.host_id)) {
      throw { code: "manual_reconnect_busy", message: "Finish the current host connection before reconnecting another host." };
    }
    if (!pending) {
      const handler = this.handler;
      if (!handler) throw { code: "manual_reconnect_unavailable", message: "Host connection is unavailable in this window." };
      const controller = new AbortController();
      pending = { target, controller, participants: 0, result: Promise.resolve(false) };
      this.pending = pending;
      const operation = pending;
      pending.result = waitForResult(Promise.resolve().then(() =>
        controller.signal.aborted ? false : handler(target, controller.signal)), controller.signal)
        .then((connected) => connected === true)
        .finally(() => { if (this.pending === operation) this.pending = null; });
    }
    pending.participants += 1;
    try {
      const connected = await waitForResult(pending.result, signal);
      return !signal.aborted && connected === true;
    } finally {
      pending.participants -= 1;
      if (pending.participants === 0 && this.pending === pending) {
        this.pending = null;
        pending.controller.abort();
      }
    }
  };
}

interface Environment {
  coordinator: ManualReconnectCoordinator;
  request: ManualReconnectRequest;
}

const Context = createContext<Environment | null>(null);

export function ManualReconnectProvider({ children, request }: { children: ReactNode; request?: ManualReconnectRequest }) {
  const [coordinator] = useState(() => new ManualReconnectCoordinator());
  return <Context.Provider value={{ coordinator, request: request ?? coordinator.request }}>{children}</Context.Provider>;
}

export function useManualReconnect(): ManualReconnectRequest | null {
  return useContext(Context)?.request ?? null;
}

export function useManualReconnectHandler(handler: ConnectHostHandler): void {
  const coordinator = useContext(Context)?.coordinator;
  const latest = useRef(handler);
  latest.current = handler;
  useEffect(() => coordinator?.register((target, signal) => latest.current(target, signal)), [coordinator]);
}
