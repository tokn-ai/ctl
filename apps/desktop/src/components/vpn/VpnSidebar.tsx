import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { VpnController } from "../../features/vpn/useVpn";
import { errorMessage } from "../../lib/errors";
import { Icon } from "../ui/Icon";
import { VpnConnectionEditor } from "./VpnConnectionEditor";
import "./vpn.css";

interface Props {
  model: VpnController;
}

export function VpnSidebar({ model }: Props) {
  const [copied_endpoint, setCopiedEndpoint] = useState<string | null>(null);
  const [copy_error, setCopyError] = useState<string | null>(null);
  const current_connection = model.connections.find((connection) => connection.connection_id === model.status.connection_id);
  const active = model.status.state !== "stopped" || model.action !== null;
  const stopping = model.status.state === "stopping" || model.action?.kind === "stop";
  const can_connect = model.status_loaded && !model.status_stale && !active;
  const external = active && !current_connection;
  const refreshing = model.catalog_loading || model.status_loading;
  const label = model.status_stale && !model.action ? "Status unavailable" : statusLabel(model);
  const vpn_url = vpnServerLabel(model.status.vpn_url ?? (active ? current_connection?.url : null));
  const username = model.status.username ?? (active ? current_connection?.username : null);
  const unavailable_detail = !model.status_loaded && !model.status_stale
    ? "Checking…"
    : active || model.status_stale ? "Unavailable" : "—";

  useEffect(() => {
    setCopiedEndpoint(null);
    setCopyError(null);
  }, [model.status.endpoint]);

  async function copyEndpoint() {
    const endpoint = model.status.endpoint;
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
    <>
      <aside className="vpn-sidebar" aria-label="VPN connections">
        <header className="sidebar-header">
          <strong>VPN</strong>
          <span className="vpn-count">{model.connections.length}</span>
          <button className="icon-button" type="button" onClick={() => void model.refresh()} disabled={refreshing} aria-label="Refresh VPN" title="Refresh VPN">
            <Icon name="refresh" class_name={refreshing ? "vpn-refreshing" : undefined} />
          </button>
          <button className="icon-button" type="button" onClick={model.addConnection} disabled={!model.catalog_loaded || model.profile_busy} aria-label="Add VPN connection" title="Add VPN connection">
            <Icon name="plus" />
          </button>
        </header>
        <div className="vpn-sidebar-body">
          <section className={`vpn-current ${model.status.state}`} aria-label="Current VPN" aria-live="polite">
            <div className="vpn-current-heading">
              <span className={`vpn-indicator ${model.status_stale ? "stale" : model.status.state}`} aria-hidden="true" />
              <strong>{model.status_loaded || model.action || model.status_stale ? label : "Checking VPN…"}</strong>
              {model.status.endpoint ? (
                <button type="button" onClick={() => void copyEndpoint()} disabled={model.status_stale || model.status.state !== "connected"} aria-label="Copy SOCKS endpoint" title="Copy SOCKS endpoint">
                  <Icon name={copied_endpoint === model.status.endpoint ? "check" : "copy"} />
                </button>
              ) : null}
            </div>
            {active ? <p>{current_connection?.name ?? (model.status.state === "connected" ? "Connected VPN" : "VPN connection")}</p> : null}
            {model.status_stale && model.status_loaded && !model.action ? <small>Last known state: {statusLabel(model)}</small> : null}
            <dl className="vpn-current-details">
              <div>
                <dt>VPN server</dt>
                <dd>{vpn_url ?? unavailable_detail}</dd>
              </div>
              <div>
                <dt>Username</dt>
                <dd>{username ?? unavailable_detail}</dd>
              </div>
              <div>
                <dt>SOCKS5 endpoint</dt>
                <dd><code>{model.status.endpoint ?? (model.status.state === "starting" ? "Pending" : unavailable_detail)}</code></dd>
              </div>
            </dl>
            {(external || (model.status_stale && !active)) ? (
              <button type="button" className="vpn-disconnect" disabled={stopping} onClick={() => void model.stop()}>
                {stopping ? "Disconnecting…" : model.status.state === "starting" ? "Cancel connection" : "Disconnect VPN"}
              </button>
            ) : null}
          </section>
          {[model.status_error, model.action_error, model.catalog_error, copy_error].filter(Boolean).map((message, index) => (
            <p className="vpn-error" role="alert" key={`${index}-${message}`}>{message}</p>
          ))}
          {model.catalog_loading && !model.catalog_loaded ? <p className="vpn-loading" role="status">Loading saved connections…</p> : null}
          {model.catalog_loaded && model.connections.length === 0 ? (
            <div className="sidebar-state vpn-empty">
              <Icon name="vpn" size={28} class_name="empty-glyph" />
              <p>No saved VPN connections.</p>
              <button type="button" onClick={model.addConnection} disabled={model.profile_busy}>Add connection</button>
            </div>
          ) : null}
          <div className="vpn-connection-list">
            {model.connections.map((connection) => {
              const selected = active && model.status.connection_id === connection.connection_id;
              const busy = model.profile_busy;
              return (
                <section className={`vpn-connection${selected ? " active" : ""}`} key={connection.connection_id} aria-label={connection.name}>
                  <div className="vpn-connection-heading">
                    <Icon name="vpn" size={16} />
                    <strong>{connection.name}</strong>
                  </div>
                  <p className="vpn-server">{vpnServerLabel(connection.url) ?? "VPN server unavailable"}</p>
                  <small>{connection.username}</small>
                  <div className="vpn-connection-actions">
                    {selected ? (
                      <button type="button" disabled={stopping} onClick={() => void model.stop()} aria-label={`${model.status.state === "starting" ? "Cancel connection to" : "Disconnect"} ${connection.name}`}>
                        {stopping ? "Disconnecting…" : model.status.state === "starting" ? "Cancel connection" : "Disconnect"}
                      </button>
                    ) : (
                      <button type="button" disabled={!can_connect || busy || !connection.has_password} onClick={() => void model.connect(connection.connection_id)} aria-label={`Connect ${connection.name}`}>
                        Connect
                      </button>
                    )}
                    <button type="button" disabled={busy || selected} onClick={() => model.editConnection(connection)} aria-label={`Edit ${connection.name}`} title={selected ? "Disconnect before editing this connection." : "Edit connection"}>Edit</button>
                    <button type="button" disabled={busy || selected} onClick={() => void model.deleteConnection(connection.connection_id)} aria-label={`Delete ${connection.name}`}>
                      {model.deleting_id === connection.connection_id ? "Deleting…" : "Delete"}
                    </button>
                  </div>
                </section>
              );
            })}
          </div>
        </div>
        <footer className="vpn-footer">
          <small>{model.last_checked_at === null ? "VPN status has not been checked." : `${model.status_stale ? "Last checked" : "Updated"} ${new Date(model.last_checked_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`}</small>
        </footer>
      </aside>
      {model.editor ? createPortal(
        <VpnConnectionEditor
          key={model.editor.editor_id}
          connection={model.editor.connection}
          saving={model.editor_saving}
          error={model.editor_error}
          on_save={model.saveConnection}
          on_close={model.closeEditor}
        />,
        document.body,
      ) : null}
    </>
  );
}

function statusLabel(model: VpnController): string {
  switch (model.status.state) {
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
