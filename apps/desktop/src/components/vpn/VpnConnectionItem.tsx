import { useEffect, useState } from "react";
import type { VpnController } from "../../features/vpn/useVpn";
import { errorMessage } from "../../lib/errors";
import { vpnNeedsSignIn, vpnRuntimeId } from "../../features/vpn/status";
import type { VpnConnection, VpnState, VpnStatus } from "../../lib/types";
import { Icon } from "../ui/Icon";

interface Props {
  connection: VpnConnection | null;
  runtime: VpnStatus | null;
  model: VpnController;
}

export function VpnConnectionItem({ connection, runtime, model }: Props) {
  const [copied_endpoint, setCopiedEndpoint] = useState<string | null>(null);
  const [copy_error, setCopyError] = useState<string | null>(null);
  const vpn_id = runtime ? vpnRuntimeId(runtime) : connection?.connection_id ?? "legacy";
  const uncertain = model.uncertain_ids.has(vpn_id) || runtime?.status_unavailable === true;
  const action = model.actions.get(vpn_id);
  const connecting = action?.kind === "connect";
  const active = runtime !== null || uncertain || action !== undefined;
  const action_error = model.action_errors.get(vpn_id);
  const state = action?.kind === "stop" ? "stopping" : action?.kind === "connect" && !runtime ? "starting" : runtime?.state ?? "stopped";
  const name = connection?.name ?? (state === "connected" ? "Connected VPN" : "VPN connection");
  const stopping = state === "stopping";
  const checking = (!model.status_loaded || uncertain) && !action;
  const stale = (model.status_stale || (uncertain && runtime !== null)) && !action;
  const needs_sign_in = state === "starting" && vpnNeedsSignIn(runtime);
  const signing_in = model.signing_in_ids.has(vpn_id);
  const status_label = stale ? "Status unavailable" : checking ? "Checking…" : needs_sign_in ? "Sign-in required" : statusLabel(state);
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
        <span className="vpn-provider">{tailscale ? "Tailscale" : "OpenConnect"}</span>
      </div>
      <div className="vpn-connection-status" aria-live="polite">
        <span className={`vpn-indicator ${stale ? "stale" : checking ? "unknown" : state}`} aria-hidden="true" />
        <span>{status_label}</span>
      </div>
      {!connection ? <small>{source_note}</small> : null}
      {runtime?.shared_container ? <small>{foreign ? "Shared VPN · This ctld is not keeping it connected." : runtime.locally_connected === true ? "Shared VPN · Kept connected by this ctld." : "Shared VPN"}</small> : null}
      {runtime && stale && model.status_loaded ? <small>Last known state: {statusLabel(runtime.state)}</small> : null}
      <dl className="vpn-connection-details">
        {tailscale ? <>
          <div><dt>Device name in Tailscale</dt><dd>{hostname ?? "Assigned when connected"}</dd></div>
          {runtime?.tailnet ? <div><dt>Tailnet</dt><dd>{runtime.tailnet}</dd></div> : null}
        </> : <div>
          <dt>VPN server</dt>
          <dd>{vpn_url ?? "Unavailable"}</dd>
        </div>}
        <div>
          <dt>Username</dt>
          <dd>{username ?? "Unavailable"}</dd>
        </div>
        {active ? (
          <div>
            <dt className="vpn-endpoint-heading">
              <span>SOCKS5 endpoint</span>
              {endpoint ? (
                <button type="button" onClick={() => void copyEndpoint()} disabled={model.status_stale || uncertain || state !== "connected"} aria-label="Copy SOCKS endpoint" title="Copy SOCKS endpoint">
                  <Icon name={copied_endpoint === endpoint ? "check" : "copy"} size={12} />
                </button>
              ) : null}
            </dt>
            <dd><code>{endpoint ?? (state === "starting" ? "Pending" : "Unavailable")}</code></dd>
          </div>
        ) : null}
      </dl>
      {needs_sign_in && !stale ? <small>Finish signing in with your browser. The login is kept for future connections.</small> : null}
      {runtime?.message ? <small>{runtime.message}</small> : null}
      {connection && !provider_supported && model.status_loaded ? <small>Update ctld to connect with {tailscale ? "Tailscale" : "OpenConnect"}.</small> : null}
      {[action_error, copy_error].filter(Boolean).map((message, index) => (
        <p className="vpn-error" role="alert" key={`${index}-${message}`}>{message}</p>
      ))}
      <div className="vpn-connection-actions">
        {needs_sign_in ? <button
          type="button"
          disabled={stale || checking || stopping || signing_in}
          onClick={() => void model.signIn(vpn_id)}
          aria-label={`Sign in to ${connection?.name ?? "Tailscale"}`}
        >{signing_in ? "Opening browser…" : "Sign in"}</button> : null}
        {active ? (
          <button
            type="button"
            disabled={stopping || (foreign && !connecting) || !model.supports_multiple}
            title={foreign && !connecting ? "This ctld has no connection to release." : !model.supports_multiple ? "Update ctld to disconnect this VPN safely." : undefined}
            onClick={() => void model.stop(vpn_id)}
            aria-label={connection
              ? `${state === "starting" || connecting ? "Cancel connection to" : "Disconnect"} ${connection.name}`
              : state === "starting" || connecting ? "Cancel connection" : "Disconnect VPN"}
          >
            {stopping ? "Disconnecting…" : state === "starting" || connecting ? "Cancel connection" : "Disconnect"}
          </button>
        ) : null}
        {connection && (!active || foreign) ? (
          <button type="button" disabled={!can_connect || model.profile_busy || (!tailscale && !saved_openconnect?.has_password)} onClick={() => void model.connect(connection.connection_id)} aria-label={`Connect ${connection.name}`}>
            {action?.kind === "connect" ? "Connecting…" : "Connect"}
          </button>
        ) : null}
        {connection ? (
          <>
            <button type="button" disabled={model.profile_busy || active || model.discovery_warnings.length > 0} onClick={() => model.editConnection(connection)} aria-label={`Edit ${connection.name}`} title={active ? "Wait until this VPN container stops before editing its connection." : "Edit connection"}>Edit</button>
            <button type="button" disabled={model.profile_busy || active || model.discovery_warnings.length > 0} onClick={() => void model.deleteConnection(connection.connection_id)} aria-label={`Delete ${connection.name}`}>
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
