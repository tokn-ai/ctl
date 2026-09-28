import { useEffect, useState } from "react";
import type { VpnController } from "../../features/vpn/useVpn";
import { errorMessage } from "../../lib/errors";
import type { VpnConnection, VpnState } from "../../lib/types";
import { Icon } from "../ui/Icon";

interface Props {
  connection: VpnConnection | null;
  active: boolean;
  model: VpnController;
}

export function VpnConnectionItem({ connection, active, model }: Props) {
  const [copied_endpoint, setCopiedEndpoint] = useState<string | null>(null);
  const [copy_error, setCopyError] = useState<string | null>(null);
  const state = active
    ? model.action?.kind === "stop" ? "stopping"
      : model.status.state === "stopped" && model.action?.kind === "connect" ? "starting"
        : model.status.state
    : "stopped";
  const name = connection?.name ?? (state === "connected" ? "Connected VPN" : "VPN connection");
  const stopping = state === "stopping";
  const checking = !model.status_loaded && !(active && model.action);
  const stale = model.status_stale && !(active && model.action);
  const status_label = stale ? "Status unavailable" : checking ? "Checking…" : statusLabel(state);
  const endpoint = active ? model.status.endpoint : null;
  const vpn_url = vpnServerLabel(active ? model.status.vpn_url ?? connection?.url : connection?.url);
  const username = active ? model.status.username ?? connection?.username : connection?.username;
  const can_connect = model.status_loaded && !model.status_stale && model.status.state === "stopped" && !model.action;
  const source_note = model.catalog_loading && !model.catalog_loaded ? "Loading saved connections…"
    : !model.catalog_loaded || model.catalog_error ? "Saved connection unavailable"
      : "Not saved in this app";

  useEffect(() => {
    setCopiedEndpoint(null);
    setCopyError(null);
  }, [endpoint]);

  async function copyEndpoint() {
    if (!endpoint) return;
    try {
      if (!navigator.clipboard?.writeText) throw new Error("Clipboard access is unavailable.");
      await navigator.clipboard.writeText(endpoint);
      setCopiedEndpoint(endpoint);
      setCopyError(null);
    } catch (failure) {
      setCopyError(errorMessage(failure));
    }
  }

  return (
    <section className={`vpn-connection${active ? " active" : ""}`} aria-label={name}>
      <div className="vpn-connection-heading">
        <Icon name="vpn" size={16} />
        <strong>{name}</strong>
      </div>
      <div className="vpn-connection-status" aria-live="polite">
        <span className={`vpn-indicator ${stale ? "stale" : checking ? "unknown" : state}`} aria-hidden="true" />
        <span>{status_label}</span>
      </div>
      {!connection ? <small>{source_note}</small> : null}
      {active && stale && model.status_loaded ? <small>Last known state: {statusLabel(model.status.state)}</small> : null}
      <dl className="vpn-connection-details">
        <div>
          <dt>VPN server</dt>
          <dd>{vpn_url ?? "Unavailable"}</dd>
        </div>
        <div>
          <dt>Username</dt>
          <dd>{username ?? "Unavailable"}</dd>
        </div>
        {active ? (
          <div>
            <dt className="vpn-endpoint-heading">
              <span>SOCKS5 endpoint</span>
              {endpoint ? (
                <button type="button" onClick={() => void copyEndpoint()} disabled={model.status_stale || state !== "connected"} aria-label="Copy SOCKS endpoint" title="Copy SOCKS endpoint">
                  <Icon name={copied_endpoint === endpoint ? "check" : "copy"} size={12} />
                </button>
              ) : null}
            </dt>
            <dd><code>{endpoint ?? (state === "starting" ? "Pending" : "Unavailable")}</code></dd>
          </div>
        ) : null}
      </dl>
      {copy_error ? <p className="vpn-error" role="alert">{copy_error}</p> : null}
      <div className="vpn-connection-actions">
        {active ? (
          <button
            type="button"
            disabled={stopping}
            onClick={() => void model.stop()}
            aria-label={connection
              ? `${state === "starting" ? "Cancel connection to" : "Disconnect"} ${connection.name}`
              : state === "starting" ? "Cancel connection" : "Disconnect VPN"}
          >
            {stopping ? "Disconnecting…" : state === "starting" ? "Cancel connection" : "Disconnect"}
          </button>
        ) : connection ? (
          <button type="button" disabled={!can_connect || model.profile_busy || !connection.has_password} onClick={() => void model.connect(connection.connection_id)} aria-label={`Connect ${connection.name}`}>
            Connect
          </button>
        ) : null}
        {connection ? (
          <>
            <button type="button" disabled={model.profile_busy || active} onClick={() => model.editConnection(connection)} aria-label={`Edit ${connection.name}`} title={active ? "Disconnect before editing this connection." : "Edit connection"}>Edit</button>
            <button type="button" disabled={model.profile_busy || active} onClick={() => void model.deleteConnection(connection.connection_id)} aria-label={`Delete ${connection.name}`}>
              {model.deleting_id === connection.connection_id ? "Deleting…" : "Delete"}
            </button>
          </>
        ) : null}
      </div>
    </section>
  );
}

function statusLabel(state: VpnState): string {
  switch (state) {
    case "starting": return "Connecting…";
    case "connected": return "Connected";
    case "stopping": return "Disconnecting…";
    case "stopped": return "Disconnected";
  }
}

function vpnServerLabel(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const gateway = new URL(value.includes("://") ? value : `https://${value}`);
    return gateway.protocol === "https:" ? gateway.origin : null;
  } catch {
    return null;
  }
}
