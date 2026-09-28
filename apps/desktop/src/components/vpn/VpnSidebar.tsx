import { createPortal } from "react-dom";
import type { VpnController } from "../../features/vpn/useVpn";
import { Icon } from "../ui/Icon";
import { VpnConnectionEditor } from "./VpnConnectionEditor";
import { VpnConnectionItem } from "./VpnConnectionItem";
import "./vpn.css";

interface Props {
  model: VpnController;
}

export function VpnSidebar({ model }: Props) {
  const recovery_stop = model.status_stale && model.action?.kind === "stop" &&
    !model.status.connection_id && !model.status.container_name && !model.status.endpoint &&
    !model.status.vpn_url && !model.status.username;
  const active = !recovery_stop && (model.status.state !== "stopped" || model.action !== null);
  const active_connection_id = model.status.connection_id ?? (model.action?.kind === "connect" ? model.action.connection_id : null);
  const current_connection = active ? model.connections.find((connection) => connection.connection_id === active_connection_id) : null;
  const external = active && !current_connection;
  const item_count = model.connections.length + Number(external);
  const refreshing = model.catalog_loading || model.status_loading;

  return (
    <>
      <aside className="vpn-sidebar" aria-label="VPN connections">
        <header className="sidebar-header">
          <strong>VPN</strong>
          <span className="vpn-count">{item_count}</span>
          <button className="icon-button" type="button" onClick={() => void model.refresh()} disabled={refreshing} aria-label="Refresh VPN" title="Refresh VPN">
            <Icon name="refresh" class_name={refreshing ? "vpn-refreshing" : undefined} />
          </button>
          <button className="icon-button" type="button" onClick={model.addConnection} disabled={!model.catalog_loaded || model.profile_busy} aria-label="Add VPN connection" title="Add VPN connection">
            <Icon name="plus" />
          </button>
        </header>
        <div className="vpn-sidebar-body">
          {!active && (!model.status_loaded || model.status_stale) ? (
            <div className="vpn-notice">
              <p role="status">{model.status_stale ? "VPN status unavailable." : "Checking VPN…"}</p>
              {model.status_stale ? (
                <button type="button" disabled={recovery_stop} onClick={() => void model.stop()}>
                  {recovery_stop ? "Disconnecting…" : "Disconnect VPN"}
                </button>
              ) : null}
            </div>
          ) : null}
          {[model.status_error, model.action_error, model.catalog_error].filter(Boolean).map((message, index) => (
            <p className="vpn-error" role="alert" key={`${index}-${message}`}>{message}</p>
          ))}
          {model.catalog_loading && !model.catalog_loaded && !external ? <p className="vpn-loading" role="status">Loading saved connections…</p> : null}
          <div className="vpn-connection-list">
            {external ? <VpnConnectionItem connection={null} active model={model} /> : null}
            {model.connections.map((connection) => (
              <VpnConnectionItem
                key={connection.connection_id}
                connection={connection}
                active={active && connection.connection_id === active_connection_id}
                model={model}
              />
            ))}
          </div>
          {model.catalog_loaded && item_count === 0 ? (
            <div className="sidebar-state vpn-empty">
              <Icon name="vpn" size={28} class_name="empty-glyph" />
              <p>No saved VPN connections.</p>
              <button type="button" onClick={model.addConnection} disabled={model.profile_busy}>Add connection</button>
            </div>
          ) : null}
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
