import { useLocale } from "../../features/i18n/LocaleContext";
import { message, translate, type Locale, type DisplayText, type LocalizedMessage } from "../../features/i18n/messages";
import { useEffect, useState } from "react";
import { errorCode, localizedErrorText } from "../../lib/errors";
import {
  checkLocalPort,
  listRemoteListeners,
} from "../../lib/tauri";
import type {
  LocalPortAvailability,
  PortForwardProtocol,
  SshConnectionTarget,
  TcpListener,
  WorkspacePortForward,
} from "../../lib/types";
import { targetLabel } from "../../features/targets/targets";
import { portForwardProtocolLabel } from "../../features/portForwarding/protocol";
import type { PortForwardFeedbackOptions, PortForwardObservation } from "../../features/portForwarding/usePortForwarding";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { InlineFeedback } from "../feedback/InlineFeedback";
import "../portForwarding/portForwarding.css";

interface Props {
  target: SshConnectionTarget;
  forwards: WorkspacePortForward[];
  statuses: ReadonlyMap<string, PortForwardObservation>;
  busy: ReadonlySet<string>;
  onChange(forwards: WorkspacePortForward[]): void;
  onSetEnabled(
    forward: WorkspacePortForward,
    enabled: boolean,
    options?: PortForwardFeedbackOptions,
  ): Promise<void>;
  onUpdateAgent(): void;
  onClose(): void;
}

export function PortForwardingDialog({
  target,
  forwards,
  statuses,
  busy,
  onChange,
  onSetEnabled,
  onUpdateAgent,
  onClose,
}: Props) {
  const { t, locale, formatText } = useLocale();
  const windows_target = target.remote_platform === "windows";
  const [error, setError] = useState<LocalizedMessage | null>(null);
  const [action_errors, setActionErrors] = useState<ReadonlyMap<string, { message: DisplayText; enabled: boolean }>>(new Map());
  const [name, setName] = useState("");
  const [localPort, setLocalPort] = useState("");
  const [remoteHost, setRemoteHost] = useState("127.0.0.1");
  const [remotePort, setRemotePort] = useState("");
  const [selected_protocol, setProtocol] = useState<PortForwardProtocol>("tcp");
  const protocol = windows_target ? "tcp" : selected_protocol;
  const [listeners, setListeners] = useState<TcpListener[]>([]);
  const [listenerWarnings, setListenerWarnings] = useState<string[]>([]);
  const [listenerError, setListenerError] = useState<DisplayText | null>(null);
  const [listenerUpdateRequired, setListenerUpdateRequired] = useState(false);
  const [listenersLoading, setListenersLoading] = useState(true);
  const [availability, setAvailability] = useState<LocalPortAvailability | null>(null);

  async function refreshListeners() {
    setListenersLoading(true);
    setListenerError(null);
    setListenerUpdateRequired(false);
    try {
      const catalog = await listRemoteListeners(target);
      setListeners([...catalog.listeners].sort((left, right) =>
        left.port - right.port || left.bind_address.localeCompare(right.bind_address) ||
        (left.protocol ?? "tcp").localeCompare(right.protocol ?? "tcp"),
      ));
      setListenerWarnings(catalog.warnings);
    } catch (failure) {
      if (errorCode(failure) === "ctl_agent_update_required") {
        setListeners([]);
        setListenerWarnings([]);
        setListenerUpdateRequired(true);
      } else {
        setListenerError(localizedErrorText(failure));
      }
    } finally {
      setListenersLoading(false);
    }
  }

  useEffect(() => {
    if (!windows_target) void refreshListeners();
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
      void checkLocalPort(port, protocol).then(
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
  }, [localPort, protocol]);

  async function setEnabled(forward: WorkspacePortForward, value: boolean) {
    setActionErrors((current) => {
      const next = new Map(current);
      next.delete(forward.forward_id);
      return next;
    });
    try {
      await onSetEnabled(forward, value, { feedback: "inline" });
    } catch (failure) {
      setActionErrors((current) => new Map(current).set(forward.forward_id, { message: localizedErrorText(failure), enabled: value }));
      throw failure;
    }
  }

  async function remove(forward: WorkspacePortForward) {
    if (forward.enabled) {
      try {
        await setEnabled(forward, false);
      } catch {
        return;
      }
    }
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
          ? message("ports.local_port_in_use", { port: parsedLocal })
          : message("ports.invalid"),
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
        protocol,
        enabled: false,
      },
    ]);
    setName("");
    setLocalPort("");
    setRemotePort("");
    setProtocol("tcp");
    setError(null);
  }

  function selectListener(listener: TcpListener) {
    const host = remoteHostForListener(listener.bind_address);
    setName(t("ports.port_name", { port: listener.port }));
    setLocalPort(String(listener.port));
    setRemoteHost(host);
    setRemotePort(String(listener.port));
    setProtocol(listener.protocol ?? "tcp");
    setError(null);
  }

  return (
    <QuickInputFrame title={t("ports.title")} onDismiss={onClose} className="port-forward-dialog">
      <header className="quick-input-heading">
        <strong>{t("ports.title_target", { host: targetLabel(target, locale) })}</strong>
        <button type="button" onClick={onClose}>{t("ports.close")}</button>
      </header>
      <p className="quick-input-description">{t("ports.loopback_hint")}
      </p>
      {!windows_target ? <section className="remote-listeners" aria-labelledby="remote-listeners-heading">
        <header>
          <strong id="remote-listeners-heading">{t("ports.remote_listeners")}</strong>
          <button type="button" onClick={() => void refreshListeners()} disabled={listenersLoading}>
            {listenersLoading ? t("ports.scanning") : t("ports.refresh")}
          </button>
        </header>
        {listenerUpdateRequired ? (
          <div className="remote-agent-update" role="status">
            <p>{t("ports.remote_update_hint")}</p>
            <button type="button" onClick={onUpdateAgent}>{t("ports.update_remote")}</button>
          </div>
        ) : null}
        {listenerError ? <InlineFeedback action={{ label: t("ports.retry_listeners"), on_click: () => void refreshListeners(), disabled: listenersLoading }}>{formatText(listenerError)}</InlineFeedback> : null}
        {!listenerError && !listenerUpdateRequired && !listenersLoading && listeners.length === 0 ? (
          <p className="remote-listener-message">{t("ports.no_listeners")}</p>
        ) : null}
        {listeners.length > 0 ? (
          <div className="remote-listener-grid">
            {listeners.map((listener) => (
              <button
                type="button"
                key={`${listener.protocol ?? "tcp"}:${listener.bind_address}:${listener.port}`}
                onClick={() => selectListener(listener)}
                title={t("ports.prefill")}
              >
                <code>{listener.bind_address}:{listener.port}</code>
                <small>{portForwardProtocolLabel(listener.protocol)} · {listenerScope(listener.bind_address, locale)}</small>
                <span>{t("ports.forward")}</span>
              </button>
            ))}
          </div>
        ) : null}
        {listenerWarnings.map((warning) => (
          <InlineFeedback severity="warning" key={warning}>{warning}</InlineFeedback>
        ))}
      </section> : null}
      <div className="port-forward-list">
        {forwards.length === 0 ? (
          <p className="port-forward-empty">{t("ports.no_host_forwards")}</p>
        ) : forwards.map((forward) => {
          const status = statuses.get(forward.forward_id);
          const changing = busy.has(forward.forward_id);
          const action_error = action_errors.get(forward.forward_id);
          const state_label = changing ? t("ports.changing") : t(`ports.state_${status?.state ?? "stopped"}`);
          return (
            <div className="port-forward-row" key={forward.forward_id}>
              <div>
                <strong>{forward.name}</strong>
                <code>{portForwardProtocolLabel(forward.protocol)} · {forward.bind_address}:{forward.local_port} → {forward.remote_host}:{forward.remote_port}</code>
                <small className={`port-forward-state ${status?.state ?? "stopped"}`}>
                  {status?.message ? t("ports.status_detail", { status: state_label, detail: status.message }) : state_label}
                </small>
                {action_error ? <InlineFeedback action={{ label: t("ports.retry_forward"), on_click: () => void setEnabled(forward, action_error.enabled).catch(() => undefined), disabled: changing }}>{formatText(action_error.message)}</InlineFeedback> : null}
              </div>
              <button
                type="button"
                disabled={changing}
                onClick={() => void setEnabled(forward, !forward.enabled).catch(() => undefined)}
              >
                {forward.enabled ? t("ports.stop") : t("ports.start")}
              </button>
              <button
                type="button"
                disabled={changing}
                onClick={() => void remove(forward)}
                aria-label={t("ports.delete_forward", { name: forward.name })}
              >{t("ports.delete")}
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
        <label>{t("ports.name")}<input value={name} onChange={(event) => setName(event.target.value)} placeholder={t("ports.example_name")} /></label>
        <label>{t("ports.protocol")}
          <select value={protocol} onChange={(event) => setProtocol(event.target.value as PortForwardProtocol)}>
            <option value="tcp">TCP</option>
            {!windows_target ? <>
              <option value="udp">UDP</option>
              <option value="any">{t("ports.any_protocol")}</option>
            </> : null}
          </select>
        </label>
        <label>{t("ports.local_port")}
          <input inputMode="numeric" value={localPort} onChange={(event) => setLocalPort(event.target.value)} placeholder="5432" />
          {availability ? (
            <small className={availability.available ? "available" : "unavailable"}>
              {availability.available ? t("ports.available") : t("ports.in_use")}
            </small>
          ) : null}
        </label>
        <label>{t("ports.remote_host")}<input value={remoteHost} onChange={(event) => setRemoteHost(event.target.value)} /></label>
        <label>{t("ports.remote_port")}<input inputMode="numeric" value={remotePort} onChange={(event) => setRemotePort(event.target.value)} placeholder="5432" /></label>
        <button type="submit" className="button-primary">{t("ports.add_stopped")}</button>
      </form>
      {error ? <InlineFeedback>{formatText(error)}</InlineFeedback> : null}
    </QuickInputFrame>
  );
}

export function remoteHostForListener(address: string): string {
  if (address === "0.0.0.0") return "127.0.0.1";
  if (address === "::") return "::1";
  return address;
}

export function listenerScope(address: string, locale: Locale = "en"): string {
  if (address === "127.0.0.1" || address === "::1") return translate(locale, "ports.scope_loopback");
  if (address === "0.0.0.0" || address === "::") return translate(locale, "ports.scope_all");
  return translate(locale, "ports.scope_specific");
}
