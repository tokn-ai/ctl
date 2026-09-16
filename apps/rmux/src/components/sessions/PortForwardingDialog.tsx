import { useEffect, useMemo, useState } from "react";
import { errorMessage } from "../../lib/errors";
import {
  configurePortForward,
  listPortForwards,
} from "../../lib/tauri";
import type {
  PortForwardStatus,
  SshConnectionTarget,
  WorkspacePortForward,
} from "../../lib/types";
import { targetLabel } from "../../features/targets/targets";
import { QuickInputFrame } from "../commands/QuickInputFrame";

interface Props {
  target: SshConnectionTarget;
  forwards: WorkspacePortForward[];
  onChange(forwards: WorkspacePortForward[]): void;
  onClose(): void;
}

export function PortForwardingDialog({
  target,
  forwards,
  onChange,
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
      parsedRemote > 65535
    ) {
      setError("Enter a name, remote host, and valid ports from 1 to 65535.");
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

  return (
    <QuickInputFrame title="Port forwarding" onDismiss={onClose}>
      <header className="quick-input-heading">
        <strong>Port forwarding · {targetLabel(target)}</strong>
        <button type="button" onClick={onClose}>Close</button>
      </header>
      <p className="quick-input-description">
        Local connections bind only to 127.0.0.1. Enabled forwards remain owned by ctld and are restored after SSH reconnects.
      </p>
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
        <label>Local port<input inputMode="numeric" value={localPort} onChange={(event) => setLocalPort(event.target.value)} placeholder="5432" /></label>
        <label>Remote host<input value={remoteHost} onChange={(event) => setRemoteHost(event.target.value)} /></label>
        <label>Remote port<input inputMode="numeric" value={remotePort} onChange={(event) => setRemotePort(event.target.value)} placeholder="5432" /></label>
        <button type="submit" className="button-primary">Add stopped forward</button>
      </form>
      {error ? <p className="quick-input-error" role="alert">{error}</p> : null}
    </QuickInputFrame>
  );
}
