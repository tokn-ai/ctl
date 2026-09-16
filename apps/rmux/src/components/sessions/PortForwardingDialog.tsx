import { useEffect, useMemo, useState } from "react";
import { errorCode, errorMessage } from "../../lib/errors";
import {
  checkLocalPort,
  configurePortForward,
  listPortForwards,
  listRemoteListeners,
} from "../../lib/tauri";
import type {
  LocalPortAvailability,
  PortForwardStatus,
  SshConnectionTarget,
  TcpListener,
  WorkspacePortForward,
} from "../../lib/types";
import { targetLabel } from "../../features/targets/targets";
import { QuickInputFrame } from "../commands/QuickInputFrame";

interface Props {
  target: SshConnectionTarget;
  forwards: WorkspacePortForward[];
  onChange(forwards: WorkspacePortForward[]): void;
  onUpdateAgent(): void;
  onClose(): void;
}

export function PortForwardingDialog({
  target,
  forwards,
  onChange,
  onUpdateAgent,
  onClose,
}: Props) {
  const [statuses, setStatuses] = useState<ReadonlyMap<string, PortForwardStatus>>(
    new Map(),
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set());
  const [name, setName] = useState("");
  const [localPort, setLocalPort] = useState("");
  const [remoteHost, setRemoteHost] = useState("127.0.0.1");
  const [remotePort, setRemotePort] = useState("");
  const [listeners, setListeners] = useState<TcpListener[]>([]);
  const [listenerWarnings, setListenerWarnings] = useState<string[]>([]);
  const [listenerError, setListenerError] = useState<string | null>(null);
  const [listenerUpdateRequired, setListenerUpdateRequired] = useState(false);
  const [listenersLoading, setListenersLoading] = useState(true);
  const [availability, setAvailability] = useState<LocalPortAvailability | null>(null);

  const enabled = useMemo(
    () => forwards.filter((forward) => forward.enabled),
    [forwards],
  );

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        for (const forward of enabled) {
          await configurePortForward(target, forward, true);
        }
        const next = await listPortForwards(target);
        if (!cancelled) {
          setStatuses(new Map(next.map((status) => [status.forward.forward_id, status])));
        }
      } catch (failure) {
        if (!cancelled) setError(errorMessage(failure));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [target, enabled]);

  async function refreshListeners() {
    setListenersLoading(true);
    setListenerError(null);
    setListenerUpdateRequired(false);
    try {
      const catalog = await listRemoteListeners(target);
      setListeners([...catalog.listeners].sort((left, right) =>
        left.port - right.port || left.bind_address.localeCompare(right.bind_address),
      ));
      setListenerWarnings(catalog.warnings);
    } catch (failure) {
      if (errorCode(failure) === "ctl_agent_update_required") {
        setListeners([]);
        setListenerWarnings([]);
        setListenerUpdateRequired(true);
      } else {
        setListenerError(errorMessage(failure));
      }
    } finally {
      setListenersLoading(false);
    }
  }

  useEffect(() => {
    void refreshListeners();
  }, [target]);

  useEffect(() => {
    const port = Number(localPort);
    setAvailability(null);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setAvailability(null);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void checkLocalPort(port).then(
        (result) => {
          if (!cancelled) setAvailability(result);
        },
        () => {
          if (!cancelled) setAvailability(null);
        },
      );
    }, 200);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [localPort]);

  async function setEnabled(forward: WorkspacePortForward, value: boolean) {
    setBusy((current) => new Set(current).add(forward.forward_id));
    setError(null);
    try {
      const status = await configurePortForward(target, forward, value);
      setStatuses((current) => {
        const next = new Map(current);
        if (value) next.set(forward.forward_id, status);
        else next.delete(forward.forward_id);
        return next;
      });
      onChange(
        forwards.map((item) =>
          item.forward_id === forward.forward_id
            ? { ...item, enabled: value }
            : item,
        ),
      );
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy((current) => {
        const next = new Set(current);
        next.delete(forward.forward_id);
        return next;
      });
    }
  }

  async function remove(forward: WorkspacePortForward) {
    if (forward.enabled) await setEnabled(forward, false);
    onChange(forwards.filter((item) => item.forward_id !== forward.forward_id));
  }

  function add() {
    const parsedLocal = Number(localPort);
    const parsedRemote = Number(remotePort);
    if (
      !name.trim() ||
      !remoteHost.trim() ||
      !Number.isInteger(parsedLocal) ||
      !Number.isInteger(parsedRemote) ||
      parsedLocal < 1 ||
      parsedLocal > 65535 ||
      parsedRemote < 1 ||
      parsedRemote > 65535 ||
      availability?.available === false
    ) {
      setError(
        availability?.available === false
          ? `Local port ${parsedLocal} is already in use.`
          : "Enter a name, remote host, and valid ports from 1 to 65535.",
      );
      return;
    }
    onChange([
      ...forwards,
      {
        forward_id: crypto.randomUUID(),
        host_id: target.host_id!,
        name: name.trim(),
        bind_address: "127.0.0.1",
        local_port: parsedLocal,
        remote_host: remoteHost.trim(),
        remote_port: parsedRemote,
        enabled: false,
      },
    ]);
    setName("");
    setLocalPort("");
    setRemotePort("");
    setError(null);
  }

  function selectListener(listener: TcpListener) {
    const host = remoteHostForListener(listener.bind_address);
    setName(`Port ${listener.port}`);
    setLocalPort(String(listener.port));
    setRemoteHost(host);
    setRemotePort(String(listener.port));
    setError(null);
  }

  return (
    <QuickInputFrame title="Port forwarding" onDismiss={onClose} className="port-forward-dialog">
      <header className="quick-input-heading">
        <strong>Port forwarding · {targetLabel(target)}</strong>
        <button type="button" onClick={onClose}>Close</button>
      </header>
      <p className="quick-input-description">
        Local connections bind only to 127.0.0.1. Enabled forwards remain owned by ctld and are restored after SSH reconnects.
      </p>
      <section className="remote-listeners" aria-labelledby="remote-listeners-heading">
        <header>
          <strong id="remote-listeners-heading">Remote TCP listeners</strong>
          <button type="button" onClick={() => void refreshListeners()} disabled={listenersLoading}>
            {listenersLoading ? "Scanning…" : "Refresh"}
          </button>
        </header>
        {listenerUpdateRequired ? (
          <div className="remote-agent-update" role="status">
            <p>Update the remote components to discover TCP listeners on this host.</p>
            <button type="button" onClick={onUpdateAgent}>Update remote components</button>
          </div>
        ) : null}
        {listenerError ? <p className="remote-listener-message error" role="status">{listenerError}</p> : null}
        {!listenerError && !listenerUpdateRequired && !listenersLoading && listeners.length === 0 ? (
          <p className="remote-listener-message">No visible TCP listeners.</p>
        ) : null}
        {listeners.length > 0 ? (
          <div className="remote-listener-grid">
            {listeners.map((listener) => (
              <button
                type="button"
                key={`${listener.bind_address}:${listener.port}`}
                onClick={() => selectListener(listener)}
                title="Prefill a local forward"
              >
                <code>{listener.bind_address}:{listener.port}</code>
                <small>{listenerScope(listener.bind_address)}</small>
                <span>Forward</span>
              </button>
            ))}
          </div>
        ) : null}
        {listenerWarnings.map((warning) => (
          <p className="remote-listener-message warning" key={warning}>{warning}</p>
        ))}
      </section>
      <div className="port-forward-list">
        {forwards.length === 0 ? (
          <p className="port-forward-empty">No saved forwards for this host.</p>
        ) : forwards.map((forward) => {
          const status = statuses.get(forward.forward_id);
          const changing = busy.has(forward.forward_id);
          return (
            <div className="port-forward-row" key={forward.forward_id}>
              <div>
                <strong>{forward.name}</strong>
                <code>{forward.bind_address}:{forward.local_port} → {forward.remote_host}:{forward.remote_port}</code>
                <small className={`port-forward-state ${status?.state ?? "stopped"}`}>
                  {changing ? "changing…" : status?.state.replace(/_/g, " ") ?? "stopped"}
                  {status?.message ? ` · ${status.message}` : ""}
                </small>
              </div>
              <button
                type="button"
                disabled={changing}
                onClick={() => void setEnabled(forward, !forward.enabled)}
              >
                {forward.enabled ? "Stop" : "Start"}
              </button>
              <button
                type="button"
                disabled={changing}
                onClick={() => void remove(forward)}
                aria-label={`Delete ${forward.name}`}
              >
                Delete
              </button>
            </div>
          );
        })}
      </div>
      <form
        className="port-forward-form"
        onSubmit={(event) => {
          event.preventDefault();
          add();
        }}
      >
        <label>Name<input value={name} onChange={(event) => setName(event.target.value)} placeholder="Database" /></label>
        <label>
          Local port
          <input inputMode="numeric" value={localPort} onChange={(event) => setLocalPort(event.target.value)} placeholder="5432" />
          {availability ? (
            <small className={availability.available ? "available" : "unavailable"}>
              {availability.available ? "Available" : "Already in use"}
            </small>
          ) : null}
        </label>
        <label>Remote host<input value={remoteHost} onChange={(event) => setRemoteHost(event.target.value)} /></label>
        <label>Remote port<input inputMode="numeric" value={remotePort} onChange={(event) => setRemotePort(event.target.value)} placeholder="5432" /></label>
        <button type="submit" className="button-primary">Add stopped forward</button>
      </form>
      {error ? <p className="quick-input-error" role="alert">{error}</p> : null}
    </QuickInputFrame>
  );
}

export function remoteHostForListener(address: string): string {
  if (address === "0.0.0.0") return "127.0.0.1";
  if (address === "::") return "::1";
  return address;
}

export function listenerScope(address: string): string {
  if (address === "127.0.0.1" || address === "::1") return "Loopback";
  if (address === "0.0.0.0" || address === "::") return "All interfaces";
  return "Specific interface";
}
