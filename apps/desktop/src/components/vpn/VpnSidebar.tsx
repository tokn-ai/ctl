import { useLocale } from "../../features/i18n/LocaleContext";
import { createPortal } from "react-dom";
import { vpnRuntimeId } from "../../features/vpn/status";
import type { VpnController } from "../../features/vpn/useVpn";
import { Icon } from "../ui/Icon";
import { VpnConnectionEditor } from "./VpnConnectionEditor";
import { VpnConnectionItem } from "./VpnConnectionItem";
import { InlineFeedback } from "../feedback/InlineFeedback";
import "./vpn.css";

interface Props {
  model: VpnController;
}

export function VpnSidebar({ model }: Props) {
  const { t, intl, formatText } = useLocale();
  const external = model.statuses.filter((status) => (model.enrollment_connection_id === null || status.connection_id !== model.enrollment_connection_id) && !model.connections.some((connection) => connection.connection_id === status.connection_id));
  const item_count = model.connections.length + external.length;
  const assigned_ids = new Set([...model.connections.map((connection) => connection.connection_id), ...model.statuses.map(vpnRuntimeId)]);
  const detached_errors = [...model.action_errors].filter(([vpn_id]) => !assigned_ids.has(vpn_id));
  const refreshing = model.catalog_loading || model.status_loading;
  const monitoring_errors = [...new Set([model.status_error, ...model.discovery_warnings, model.catalog_error].filter((message): message is string => Boolean(message)))];
  const monitoring_title = model.status_error || model.status_stale || model.discovery_warnings.length > 0 && model.statuses.length === 0
    ? t("vpn.sidebar_unavailable") : model.discovery_warnings.length > 0 ? t("vpn.sidebar_partial")
      : model.catalog_loaded ? t("vpn.sidebar_could_not_refresh") : t("vpn.sidebar_saved_unavailable");

  return (
    <>
      <aside className="vpn-sidebar" aria-label={t("vpn.connections")}>
        <header className="sidebar-header">
          <strong>{t("vpn.source")}</strong>
          <span className="vpn-count">{intl.formatNumber(item_count)}</span>
          <button className="icon-button" type="button" onClick={() => void model.refresh()} disabled={refreshing} aria-label={t("vpn.refresh")} title={t("vpn.refresh")}>
            <Icon name="refresh" class_name={refreshing ? "vpn-refreshing" : undefined} />
          </button>
          <button className="icon-button" type="button" onClick={model.addConnection} disabled={!model.catalog_loaded || model.profile_busy} aria-label={t("vpn.add_connection")} title={t("vpn.add_connection")}>
            <Icon name="plus" />
          </button>
        </header>
        <div className="vpn-sidebar-body">
          {model.statuses.length === 0 && !model.status_loaded && monitoring_errors.length === 0 ? (
            <div className="vpn-notice">
              <p role="status">{t("vpn.checking_sidebar")}</p>
            </div>
          ) : null}
          {model.status_loaded && !model.supports_multiple ? (
            <p className="vpn-notice">{t("vpn.multiple_requires_update")}</p>
          ) : null}
          {monitoring_errors.length > 0 || model.status_stale ? <InlineFeedback severity="warning" action={{ label: t("vpn.retry_check"), on_click: () => void model.refresh(), disabled: refreshing }}>
            <strong>{monitoring_title}</strong>
            {monitoring_errors.map((message) => <p key={message}>{message}</p>)}
            {model.catalog_error && model.catalog_loaded ? <p>{t("vpn.saved_previous")}</p> : null}
          </InlineFeedback> : null}
          {detached_errors.map(([vpn_id, message]) => <InlineFeedback key={vpn_id}>{formatText(message)}</InlineFeedback>)}
          {model.catalog_loading && !model.catalog_loaded && external.length === 0 ? <p className="vpn-loading" role="status">{t("vpn.loading_saved")}</p> : null}
          <div className="vpn-connection-list">
            {external.map((runtime) => <VpnConnectionItem key={`runtime-${vpnRuntimeId(runtime)}`} connection={null} runtime={runtime} model={model} />)}
            {model.connections.map((connection) => (
              <VpnConnectionItem
                key={connection.connection_id}
                connection={connection}
                runtime={model.statuses.find((status) => status.connection_id === connection.connection_id) ?? null}
                model={model}
              />
            ))}
          </div>
          {model.catalog_loaded && model.status_loaded && !model.status_stale && model.discovery_warnings.length === 0 && item_count === 0 ? (
            <div className="sidebar-state vpn-empty">
              <Icon name="vpn" size={28} class_name="empty-glyph" />
              <p>{t("vpn.empty")}</p>
              <button type="button" onClick={model.addConnection} disabled={model.profile_busy}>{t("vpn.add")}</button>
            </div>
          ) : null}
        </div>
        <footer className="vpn-footer">
          <small>{model.last_checked_at === null ? t("vpn.unchecked") : t("vpn.check_time", { stale: String(model.status_stale), time: intl.formatTime(model.last_checked_at, { hour: "2-digit", minute: "2-digit" }) })}</small>
        </footer>
      </aside>
      {model.editor ? createPortal(
        <VpnConnectionEditor
          key={model.editor.editor_id}
          connection={model.editor.connection}
          saving={model.editor_saving}
          error={model.editor_error}
          on_save={model.saveConnection}
          enrollment_supported={model.supports_tailscale_enrollment}
          on_enrollment_change={model.setEnrollmentConnectionId}
          on_save_enrollment={model.saveEnrollment}
          on_close={model.closeEditor}
        />,
        document.body,
      ) : null}
    </>
  );
}
