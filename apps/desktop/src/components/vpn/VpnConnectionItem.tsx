import { LocalizedError, message } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useState } from "react";
import type { VpnController } from "../../features/vpn/useVpn";
import { localizedErrorText } from "../../lib/errors";
import { vpnNeedsSignIn, vpnRuntimeId, vpnStateLabel } from "../../features/vpn/status";
import type { VpnConnection, VpnStatus } from "../../lib/types";
import { Icon } from "../ui/Icon";
import { InlineFeedback } from "../feedback/InlineFeedback";
import { useNotificationEnvironment } from "../../features/notifications/NotificationContext";

interface Props {
  connection: VpnConnection | null;
  runtime: VpnStatus | null;
  model: VpnController;
}

export function VpnConnectionItem({ connection, runtime, model }: Props) {
  const { t, locale, formatText } = useLocale();
  const notifications = useNotificationEnvironment()?.store;
  const [copied_endpoint, setCopiedEndpoint] = useState<string | null>(null);
  const vpn_id = runtime ? vpnRuntimeId(runtime) : connection?.connection_id ?? "legacy";
  const uncertain = model.uncertain_ids.has(vpn_id) || runtime?.status_unavailable === true;
  const action = model.actions.get(vpn_id);
  const connecting = action?.kind === "connect";
  const active = runtime !== null || uncertain || action !== undefined;
  const action_error = model.action_errors.get(vpn_id);
  const state = action?.kind === "stop" ? "stopping" : action?.kind === "connect" && !runtime ? "starting" : runtime?.state ?? "stopped";
  const connection_name = connection?.name ?? message(state === "connected" ? "vpn.connected_vpn" : "vpn.connection");
  const name = formatText(connection_name);
  const stopping = state === "stopping";
  const checking = (!model.status_loaded || uncertain) && !action;
  const stale = (model.status_stale || (uncertain && runtime !== null)) && !action;
  const needs_sign_in = state === "starting" && vpnNeedsSignIn(runtime);
  const signing_in = model.signing_in_ids.has(vpn_id);
  const status_label = stale ? t("vpn.status_unavailable") : checking ? t("vpn.checking") : needs_sign_in ? t("vpn.sign_in_required") : vpnStateLabel(state, locale);
  const provider = runtime?.provider ?? connection?.provider ?? "openconnect";
  const tailscale = provider === "tailscale";
  const provider_supported = model.supported_providers.includes(provider);
  const saved_openconnect = connection?.provider === "tailscale" ? null : connection;
  const hostname = runtime?.hostname ?? (connection?.provider === "tailscale" ? connection.hostname : null);
  const endpoint = runtime?.endpoint;
  const foreign = runtime?.locally_connected === false;
  const vpn_url = vpnServerLabel(runtime?.vpn_url ?? saved_openconnect?.url);
  const username = runtime?.username ?? saved_openconnect?.username;
  const can_connect = provider_supported && model.status_loaded && !model.status_stale && !action &&
    !uncertain && (model.supports_multiple || (model.statuses.length === 0 && model.actions.size === 0 && model.uncertain_ids.size === 0));
  const source_note = model.catalog_loading && !model.catalog_loaded ? t("vpn.loading_saved")
    : !model.catalog_loaded || model.catalog_error ? t("vpn.saved_unavailable")
      : t("vpn.not_saved");

  useEffect(() => {
    setCopiedEndpoint(null);
  }, [endpoint]);

  async function copyEndpoint() {
    if (!endpoint) return;
    try {
      if (!navigator.clipboard?.writeText) throw new LocalizedError("vpn.clipboard_unavailable");
      await navigator.clipboard.writeText(endpoint);
      setCopiedEndpoint(endpoint);
    } catch (failure) {
      notifications?.notify(`vpn:copy:${vpn_id}`, {
        severity: "error", title: message("vpn.copy_failed"), source: message("vpn.source"),
        message: message("vpn.named_error", { name: connection_name, error: localizedErrorText(failure) }),
      });
    }
  }

  return (
    <section className={`vpn-connection${active ? " active" : ""}`} aria-label={name}>
      <div className="vpn-connection-heading">
        <Icon name="vpn" size={16} />
        <strong>{name}</strong>
        <span className="vpn-provider">{tailscale ? "Tailscale" : "OpenConnect"}</span>
      </div>
      <div className="vpn-connection-status" aria-live="polite">
        <span className={`vpn-indicator ${stale ? "stale" : checking ? "unknown" : state}`} aria-hidden="true" />
        <span>{status_label}</span>
      </div>
      {!connection ? <small>{source_note}</small> : null}
      {runtime?.shared_container ? <small>{foreign ? t("vpn.shared_foreign") : runtime.locally_connected === true ? t("vpn.shared_local") : t("vpn.shared")}</small> : null}
      {runtime && stale && model.status_loaded ? <small>{t("vpn.last_known_state", { state: vpnStateLabel(runtime.state, locale) })}</small> : null}
      <dl className="vpn-connection-details">
        {tailscale ? <>
          <div><dt>{t("vpn.tailscale_device_name")}</dt><dd>{hostname ?? t("vpn.device_name_later")}</dd></div>
          {runtime?.tailnet ? <div><dt>{t("vpn.tailnet")}</dt><dd>{runtime.tailnet}</dd></div> : null}
        </> : <div>
          <dt>{t("vpn.server")}</dt>
          <dd>{vpn_url ?? t("vpn.unavailable")}</dd>
        </div>}
        <div>
          <dt>{t("vpn.username")}</dt>
          <dd>{username ?? t("vpn.unavailable")}</dd>
        </div>
        {active ? (
          <div>
            <dt className="vpn-endpoint-heading">
              <span>{t("vpn.socks_endpoint")}</span>
              {endpoint ? (
                <button type="button" onClick={() => void copyEndpoint()} disabled={model.status_stale || uncertain || state !== "connected"} aria-label={t("vpn.copy_endpoint")} title={t("vpn.copy_endpoint")}>
                  <Icon name={copied_endpoint === endpoint ? "check" : "copy"} size={12} />
                </button>
              ) : null}
            </dt>
            <dd><code>{endpoint ?? (state === "starting" ? t("vpn.pending") : t("vpn.unavailable"))}</code></dd>
          </div>
        ) : null}
      </dl>
      {needs_sign_in && !stale ? <small>{t("vpn.finish_sign_in_persistent")}</small> : null}
      {runtime?.message ? <small>{runtime.message}</small> : null}
      {connection && !provider_supported && model.status_loaded ? <small>{t("vpn.provider_requires_update", { provider: tailscale ? "Tailscale" : "OpenConnect" })}</small> : null}
      {action_error ? <InlineFeedback>{formatText(action_error)}</InlineFeedback> : null}
      <div className="vpn-connection-actions">
        {needs_sign_in ? <button
          type="button"
          disabled={stale || checking || stopping || signing_in}
          onClick={() => void model.signIn(vpn_id)}
          aria-label={t("vpn.sign_in_named", { name: connection?.name ?? "Tailscale" })}
        >{signing_in ? t("vpn.opening_browser") : t("vpn.sign_in")}</button> : null}
        {active ? (
          <button
            type="button"
            disabled={stopping || (foreign && !connecting) || !model.supports_multiple}
            title={foreign && !connecting ? t("vpn.no_local_connection") : !model.supports_multiple ? t("vpn.safe_disconnect_requires_update") : undefined}
            onClick={() => void model.stop(vpn_id)}
            aria-label={connection
              ? t("vpn.cancel_or_disconnect", { canceling: String(state === "starting" || connecting), name: connection.name })
              : state === "starting" || connecting ? t("vpn.cancel_connection") : t("vpn.disconnect_vpn")}
          >
            {stopping ? t("vpn.disconnecting") : state === "starting" || connecting ? t("vpn.cancel_connection") : t("vpn.disconnect")}
          </button>
        ) : null}
        {connection && (!active || foreign) ? (
          <button type="button" disabled={!can_connect || model.profile_busy || (!tailscale && !saved_openconnect?.has_password)} onClick={() => void model.connect(connection.connection_id)} aria-label={t("vpn.connect_named", { name: connection.name })}>
            {action?.kind === "connect" ? t("vpn.connecting") : t("vpn.connect")}
          </button>
        ) : null}
        {connection ? (
          <>
            <button type="button" disabled={model.profile_busy || active || model.discovery_warnings.length > 0} onClick={() => model.editConnection(connection)} aria-label={t("vpn.edit_named", { name: connection.name })} title={active ? t("vpn.edit_wait") : t("vpn.edit")}>{t("vpn.edit_action")}</button>
            <button type="button" disabled={model.profile_busy || active || model.discovery_warnings.length > 0} onClick={() => void model.deleteConnection(connection.connection_id)} aria-label={t("vpn.delete_named", { name: connection.name })}>
              {model.deleting_id === connection.connection_id ? t("vpn.deleting") : t("vpn.delete")}
            </button>
          </>
        ) : null}
      </div>
    </section>
  );
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
