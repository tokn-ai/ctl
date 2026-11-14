import { message, type DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { vpnNeedsSignIn, vpnRouteDetail } from "../../features/vpn/status";
import { isHostRouteStep, isVpnRouteStep, orderedSshRoute, resolvedVpnExecutionTarget } from "../../features/workspace/sshRoute";
import { resolveSshGateways } from "../../features/workspace/workspaceModel";
import { openVpnSignIn, stopVpn, vpnStatus } from "../../lib/tauri";
import { localizedErrorText } from "../../lib/errors";
import { useMemo, useState } from "react";
import type {
  SshConnectionTarget,
  SshGatewayMode,
  SshGatewayRouteStep,
  WorkspaceHost,
  WorkspaceSshGateway,
  SshIdentityFile,
  SshRemotePlatform,
  VpnConnection,
  VpnStatus,
} from "../../lib/types";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { InlineFeedback } from "../feedback/InlineFeedback";
import "./gatewayRoute.css";
import { SshRemotePlatformSelect } from "./SshRemotePlatformSelect";

interface Props {
  title?: string;
  submitLabel?: string;
  target: SshConnectionTarget;
  gateways: readonly WorkspaceSshGateway[];
  hosts?: readonly WorkspaceHost[];
  targets: readonly SshConnectionTarget[];
  vpn_connections?: readonly VpnConnection[];
  vpn_statuses?: readonly VpnStatus[];
  vpn_loading?: boolean;
  vpn_error?: string | null;
  hostSetup?: {
    address: string;
    default_address?: string;
    default_alias?: string;
    default_identity_file?: string;
    alias: string;
    identity_file: string;
    remote_platform?: SshRemotePlatform;
    suggestions: readonly string[];
    warning: string | null;
    identity_files?: readonly SshIdentityFile[];
    identity_loading?: boolean;
    identity_warning?: string;
    ssh_config_master?: {
      checked: boolean;
      onChange(checked: boolean): void;
    };
    export_to_ssh_config?: {
      checked: boolean;
      allowed: boolean;
      onChange(checked: boolean): void;
    };
    onAddressChange(value: string): void;
    onAliasChange(value: string): void;
    onIdentityFileChange(value: string): void;
    onRemotePlatformChange(value: SshRemotePlatform): void;
  };
  readonlyExisting?: boolean;
  readonlyGatewayIds?: readonly string[];
  requireGateway?: boolean;
  closeLabel?: string;
  onSave(
    gateways: WorkspaceSshGateway[],
    route: SshGatewayRouteStep[],
    vpn_connection_id?: string,
  ): Promise<void>;
  onClose(): void;
}

interface GatewayDraft {
  kind: "ssh" | "socks5";
  gateway_id: string;
  name: string;
  destination: string;
  hostname: string;
  user: string;
  port: string;
  identity_file: string;
}

export function GatewayRouteDialog({
  title,
  submitLabel,
  target,
  gateways,
  hosts = [],
  targets,
  vpn_connections = [],
  vpn_statuses = [],
  vpn_loading = false,
  vpn_error,
  hostSetup,
  readonlyExisting = false,
  readonlyGatewayIds,
  requireGateway = false,
  closeLabel,
  onSave,
  onClose,
}: Props) {
  const { t, locale, formatText } = useLocale();
  const [draftGateways, setDraftGateways] = useState<WorkspaceSshGateway[]>(
    () => gateways.map((gateway) => ({ ...gateway })),
  );
  const [route, setRoute] = useState<SshGatewayRouteStep[]>(() => orderedSshRoute(target));
  const onlyVpn = route.length === 1 && isVpnRouteStep(route[0]) ? route[0].vpn_connection_id : undefined;
  const [host_methods, setHostMethods] = useState<Record<string, string>>({});
  const saved_hosts = hosts.filter((host) => host.host_id !== "local" && host.host_id !== target.host_id &&
    (!host.source || host.source === "saved") && host.connection_methods.length > 0);
  const resolved_route = resolveRoute(route);
  const missingVpn = [...route.flatMap((step) => isVpnRouteStep(step) ? [step.vpn_connection_id] : []),
    ...resolved_route.gateways.flatMap((gateway) => gateway.kind === "vpn" ? [gateway.vpn_connection_id] : [])]
    .some((connection_id) => !vpn_connections.some((connection) => connection.connection_id === connection_id));
  const privateMaster = resolved_route.gateways.some((gateway) => gateway.kind === "vpn" || gateway.kind === "socks5" || Boolean(gateway.hostname)) ||
    route.some(isVpnRouteStep);
  const [editing, setEditing] = useState<GatewayDraft | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<DisplayText | null>(null);
  const usage_targets = useMemo(() => [...targets, ...hosts.flatMap((host) =>
    host.connection_methods.map((method) => ({ ...method.target, host_id: host.host_id })))], [targets, hosts]);
  const usage = useMemo(() => gatewayUsage(usage_targets, gateways, hosts), [usage_targets, gateways, hosts]);
  const otherUsage = useMemo(
    () => gatewayUsage(usage_targets.filter((item) => item.host_id !== target.host_id), gateways, hosts),
    [target.host_id, usage_targets, gateways, hosts],
  );
  const routeIds = new Set(route.flatMap((step) => "gateway_id" in step ? [step.gateway_id] : []));
  const routeHostIds = new Set(route.flatMap((step) => isHostRouteStep(step) ? [step.host_id] : []));
  const existingIds = new Set(
    readonlyGatewayIds ?? gateways.map((gateway) => gateway.gateway_id),
  );
  const lastGateway = resolved_route.gateways[resolved_route.gateways.length - 1];
  const canAddVpn = !route.length || (!resolved_route.error && lastGateway?.kind !== "vpn" && lastGateway?.kind !== "socks5");

  function resolveRoute(steps: SshGatewayRouteStep[]) {
    try {
      return {
        gateways: resolveSshGateways({ ...target, vpn_connection_id: undefined, gateway_route: steps }, draftGateways, hosts).gateways ?? [],
        error: null,
      };
    } catch (failure) {
      return { gateways: [], error: localizedErrorText(failure) };
    }
  }

  function selectedHostMethod(host: WorkspaceHost): string {
    return host_methods[host.host_id] ?? host.preferred_method_id ?? host.connection_methods[0].method_id;
  }

  function addHost(host: WorkspaceHost) {
    const next = [...route, { host_id: host.host_id, method_id: selectedHostMethod(host), mode: "automatic" as const }];
    const resolved = resolveRoute(next);
    if (resolved.error) {
      setError(resolved.error);
      return;
    }
    setError(null);
    setRoute(next);
  }

  function addExisting(gateway: WorkspaceSshGateway) {
    if (routeIds.has(gateway.gateway_id) || route.length >= 8) return;
    setRoute((current) => [
      ...current,
      { gateway_id: gateway.gateway_id, mode: "automatic" },
    ]);
  }

  function editGateway(gateway: WorkspaceSshGateway) {
    setError(null);
    setEditing(toDraft(gateway));
  }

  function saveGateway() {
    if (!editing) return;
    const gateway = fromDraft(editing);
    if (!gateway) {
      setError(message("route.invalid_fields"));
      return;
    }
    const duplicate = draftGateways.some(
      (item) =>
        item.gateway_id !== gateway.gateway_id &&
        item.name.toLocaleLowerCase() === gateway.name.toLocaleLowerCase(),
    );
    if (duplicate) {
      setError(message("route.duplicate_name"));
      return;
    }
    const existing = draftGateways.find(
      (item) => item.gateway_id === gateway.gateway_id,
    );
    const next = existing
      ? draftGateways.map((item) =>
          item.gateway_id === gateway.gateway_id
            ? {
                ...gateway,
                remote_info: sameEndpoint(item, gateway)
                  ? item.remote_info
                  : undefined,
              }
            : item,
        )
      : [...draftGateways, gateway];
    setDraftGateways(next);
    if (!existing) addExisting(gateway);
    setEditing(null);
    setError(null);
  }

  function move(index: number, offset: -1 | 1) {
    const destination = index + offset;
    if (destination < 0 || destination >= route.length) return;
    setRoute((current) => {
      const next = [...current];
      [next[index], next[destination]] = [next[destination], next[index]];
      return next;
    });
  }

  function deleteGateway(gateway: WorkspaceSshGateway) {
    const draftUsage = (otherUsage.get(gateway.gateway_id) ?? 0) +
      (routeIds.has(gateway.gateway_id) ? 1 : 0);
    if (draftUsage > 0) return;
    setDraftGateways((current) =>
      current.filter((item) => item.gateway_id !== gateway.gateway_id));
  }

  async function save() {
    if (missingVpn) {
      setError(message("route.vpn_unavailable_save"));
      return;
    }
    for (const [index, step] of route.entries()) {
      if ("gateway_id" in step && !draftGateways.some((gateway) => gateway.gateway_id === step.gateway_id)) {
        setError(message("route.gateway_unavailable_save"));
        return;
      }
      if (!isVpnRouteStep(step) || index === 0) continue;
      const preceding = route[index - 1];
      if (isVpnRouteStep(preceding) || "gateway_id" in preceding &&
        draftGateways.find((gateway) => gateway.gateway_id === preceding.gateway_id)?.kind === "socks5") {
        setError(message("route.invalid_remote_vpn"));
        return;
      }
    }
    if (resolved_route.error) {
      setError(resolved_route.error);
      return;
    }
    if (requireGateway && route.length === 0) {
      setError(message("route.gateway_required"));
      return;
    }
    setSaving(true);
    setError(null);
    try {
      if (onlyVpn) await onSave(draftGateways, [], onlyVpn);
      else await onSave(draftGateways, route);
    } catch (failure) {
      setError(localizedErrorText(failure));
      setSaving(false);
    }
  }

  if (editing) {
    const sharedCount = usage.get(editing.gateway_id) ?? 0;
    return (
      <QuickInputFrame
        title={t("route.edit_gateway")}
        onDismiss={() => setEditing(null)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            setEditing(null);
          }
        }}
        className="gateway-route-dialog"
      >
        <header className="quick-input-heading">
          <strong>{sharedCount > 1 ? t("route.edit_shared") : t("route.gateway")}</strong>
          <button type="button" onClick={() => setEditing(null)}>{t("route.back")}</button>
        </header>
        {sharedCount > 1 ? (
          <InlineFeedback severity="warning">
            {t("route.gateway_in_use", { count: sharedCount })}
          </InlineFeedback>
        ) : null}
        <GatewayForm draft={editing} onChange={setEditing} />
        {error ? <InlineFeedback>{formatText(error)}</InlineFeedback> : null}
        <footer className="gateway-dialog-actions">
          <button type="button" onClick={() => setEditing(null)}>{t("route.cancel")}</button>
          <button type="button" className="button-primary" onClick={saveGateway}>
            {t("route.save_gateway")}
          </button>
        </footer>
      </QuickInputFrame>
    );
  }

  return (
    <QuickInputFrame
      title={title ?? (hostSetup ? t("route.add_host_with_gateways") : t("route.title"))}
      onDismiss={onClose}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !saving) {
          event.preventDefault();
          onClose();
        }
      }}
      className="gateway-route-dialog"
    >
      <header className="quick-input-heading">
        <strong>{title ?? (hostSetup ? t("route.add_host_with_gateways") : t("route.for_destination", { destination: target.destination }))}</strong>
        <button type="button" onClick={onClose} disabled={saving}>{closeLabel ?? t("route.close")}</button>
      </header>
      {hostSetup ? (
        <section className="gateway-host-form" aria-label={t("route.host_details")}>
          <label>
            {t("route.ssh_address")}
            <input
              autoFocus
              list="routed-host-suggestions"
              value={hostSetup.address}
              onChange={(event) => hostSetup.onAddressChange(event.target.value)}
              placeholder={hostSetup.default_address ? t("route.preferred_address", { address: hostSetup.default_address }) : "operator@server.internal:2222"}
              aria-describedby={hostSetup.default_address ? "preferred-ssh-endpoint" : undefined}
            />
          </label>
          {hostSetup.default_address ? <p id="preferred-ssh-endpoint">
            {t("route.use_preferred")}
          </p> : null}
          <datalist id="routed-host-suggestions">
            {hostSetup.suggestions.map((suggestion) => (
              <option key={suggestion} value={suggestion} />
            ))}
          </datalist>
          <label>
            {t("route.optional_alias")}
            <input
              value={hostSetup.alias}
              onChange={(event) => hostSetup.onAliasChange(event.target.value)}
              placeholder={hostSetup.default_alias ? t("route.default_alias", { alias: hostSetup.default_alias }) : t("route.default_host")}
            />
          </label>
          <label>
            {t("route.optional_identity")}
            <input
              list="connection-identity-suggestions"
              aria-label={t("route.optional_identity")}
              value={hostSetup.identity_file}
              onChange={(event) => hostSetup.onIdentityFileChange(event.target.value)}
              placeholder={hostSetup.default_identity_file ?? "~/.ssh/id_ed25519"}
            />
            <datalist id="connection-identity-suggestions">
              {hostSetup.identity_files?.map((file) => <option key={file.path} value={file.path}>{file.display_path}</option>)}
            </datalist>
            {hostSetup.identity_loading ? <span>{t("route.loading_identity")}</span> : null}
            {hostSetup.identity_warning ? <span role="status">{hostSetup.identity_warning}</span> : null}
          </label>
          <SshRemotePlatformSelect value={hostSetup.remote_platform} onChange={hostSetup.onRemotePlatformChange} />
          {hostSetup.ssh_config_master ? (
            <label className="gateway-checkbox-option">
              <input
                type="checkbox"
                aria-label={t("route.ssh_master")}
                aria-describedby="ssh-config-master-description"
                checked={hostSetup.ssh_config_master.checked && !privateMaster}
                disabled={privateMaster}
                onChange={(event) => hostSetup.ssh_config_master?.onChange(event.target.checked)}
              />
              {t("route.ssh_master")}
              <small id="ssh-config-master-description">
                {privateMaster
                  ? t("route.private_master_route")
                  : hostSetup.ssh_config_master.checked
                    ? t("route.ssh_master_description")
                    : t("route.private_master")}
              </small>
            </label>
          ) : null}
          {hostSetup.export_to_ssh_config ? (
            <label className="gateway-checkbox-option">
              <input
                type="checkbox"
                aria-label={t("route.export_ssh")}
                checked={hostSetup.export_to_ssh_config.checked && route.length === 0 && hostSetup.export_to_ssh_config.allowed}
                disabled={route.length > 0 || !hostSetup.export_to_ssh_config.allowed}
                onChange={(event) => hostSetup.export_to_ssh_config?.onChange(event.target.checked)}
              />
              {t("route.export_ssh")}
              {route.length > 0 || !hostSetup.export_to_ssh_config.allowed
                ? <small>{t("route.export_ssh_description")}</small>
                : null}
            </label>
          ) : null}
          {hostSetup.warning ? <p role="status">{hostSetup.warning}</p> : null}
        </section>
      ) : null}
      <div className="gateway-host-form">
        <label>
          {t("route.connect_through")}
          <select
            aria-label={t("route.connect_through")}
            value={onlyVpn ? `vpn:${onlyVpn}` : route.length ? "gateway_route" : "direct"}
            onChange={(event) => {
              const value = event.target.value;
              setError(null);
              if (value === "gateway_route") return;
              setRoute(value.startsWith("vpn:") ? [{ vpn_connection_id: value.slice(4) }]
                : value.startsWith("gateway:") ? [{ gateway_id: value.slice(8), mode: "automatic" }] : []);
            }}
          >
            <option value="direct">{t("route.direct")}</option>
            {route.length ? <option value="gateway_route">{t("route.hop_count", { count: route.length })}</option> : null}
            {onlyVpn && missingVpn ? <option value={`vpn:${onlyVpn}`}>{t("route.missing_vpn")}</option> : null}
            {vpn_connections.length ? <optgroup label={t("route.saved_vpns")}>
              {vpn_connections.map((connection) => <option key={connection.connection_id} value={`vpn:${connection.connection_id}`}>
                {connection.name} · {vpnRouteDetail(connection, vpn_statuses, locale)}
              </option>)}
            </optgroup> : null}
            {draftGateways.length ? <optgroup label={t("route.saved_gateways")}>
              {draftGateways.map((gateway) => <option key={gateway.gateway_id} value={`gateway:${gateway.gateway_id}`}>
                {gateway.name} · {gateway.kind === "socks5" ? "SOCKS5" : "SSH"}
              </option>)}
            </optgroup> : null}
          </select>
        </label>
        {vpn_loading ? <p role="status">{t("route.loading_vpn")}</p> : null}
        {vpn_error ? <InlineFeedback severity="warning">{t("route.vpn_load_failed", { detail: vpn_error })}</InlineFeedback> : null}
        {missingVpn ? <InlineFeedback>{t("route.vpn_unavailable_save")}</InlineFeedback> : null}
      </div>
      <p className="quick-input-description">
        {t("route.description")}
      </p>

      <div className="gateway-route-path">
        <RouteNode label={t("route.computer")} detail={t("route.start")} />
        {route.map((step, index) => {
          if (isVpnRouteStep(step)) {
            const vpn = vpn_connections.find((connection) => connection.connection_id === step.vpn_connection_id);
            const previousSsh = [...resolveRoute(route.slice(0, index)).gateways].reverse()
              .find((gateway) => gateway.kind !== "vpn" && gateway.kind !== "socks5");
            const execution = resolveRoute(route.slice(0, index + 1)).gateways;
            const owner = resolvedVpnExecutionTarget(execution, execution.length - 1);
            const ownerLabel = previousSsh?.name ?? t("route.computer");
            const name = vpn?.name ?? t("route.missing_vpn");
            return (
              <div className="gateway-route-step" key={`vpn:${index}:${step.vpn_connection_id}`}>
                <div className="gateway-route-connector" aria-hidden="true">↓</div>
                <div className="gateway-route-card">
                  <div><strong>{index + 1}. {name}</strong><small>{t("route.vpn_on", { owner: ownerLabel })}</small></div>
                  <div className="gateway-route-controls">
                    <button type="button" onClick={() => move(index, -1)} disabled={index === 0} aria-label={t("route.move_up", { name: name })}>↑</button>
                    <button type="button" onClick={() => move(index, 1)} disabled={index === route.length - 1} aria-label={t("route.move_down", { name: name })}>↓</button>
                    <button type="button" onClick={() => setRoute((current) => current.filter((_, itemIndex) => itemIndex !== index))} aria-label={t("route.remove_from_route", { name: name })}>{t("route.remove")}</button>
                  </div>
                  {owner && vpn ? <RemoteVpnControls key={JSON.stringify(owner)} connection={vpn} owner={owner} owner_label={ownerLabel} />
                    : index === 0 && vpn ? <small>{t("route.manage_vpn", { detail: vpnRouteDetail(vpn, vpn_statuses, locale) })}</small> : null}
                </div>
              </div>
            );
          }
          if (isHostRouteStep(step)) {
            const host = hosts.find((candidate) => candidate.host_id === step.host_id);
            const method = host?.connection_methods.find((candidate) => candidate.method_id === step.method_id);
            const name = host?.name ?? t("route.missing_host");
            const prefix = resolveRoute(route.slice(0, index + 1));
            const inherited_start = resolveRoute(route.slice(0, index)).gateways.length;
            const inherited = prefix.gateways.slice(inherited_start, -1);
            return <div className="gateway-route-step" key={`host:${index}:${step.host_id}`}>
              <div className="gateway-route-connector" aria-hidden="true">↓</div>
              <div className="gateway-route-card">
                <div>
                  <strong>{index + 1}. {name}</strong>
                  <small>{t("route.saved_host_method", { method: method?.name ?? t("route.missing_method") })}</small>
                  {method ? <small>{endpointLabel({ ...method.target, gateway_id: step.host_id, name })}</small> : null}
                  <small>{t("route.linked_method")}</small>
                  {inherited.length ? <small>{t("route.via_vpn", { vpn: inherited.map((gateway) => gateway.kind === "vpn"
                    ? vpn_connections.find((connection) => connection.connection_id === gateway.vpn_connection_id)?.name ?? t("route.missing_vpn")
                    : gateway.name).join(" → ") })}</small> : null}
                  {prefix.error ? <small role="status">{formatText(prefix.error)}</small> : null}
                </div>
                {inherited.map((gateway, inherited_index) => {
                  if (gateway.kind !== "vpn") return null;
                  const vpn = vpn_connections.find((connection) => connection.connection_id === gateway.vpn_connection_id);
                  if (!vpn) return null;
                  const expanded_index = inherited_start + inherited_index;
                  const owner = resolvedVpnExecutionTarget(prefix.gateways, expanded_index);
                  const owner_label = prefix.gateways[expanded_index - 1]?.name ?? t("route.computer");
                  return <div className="gateway-inherited-vpn" key={`inherited-vpn:${expanded_index}`}>
                    <small>{t("route.inherited_vpn", { name: vpn.name, owner: owner_label })}</small>
                    {owner ? <RemoteVpnControls key={JSON.stringify(owner)} connection={vpn} owner={owner} owner_label={owner_label} />
                      : <small>{t("route.manage_vpn", { detail: vpnRouteDetail(vpn, vpn_statuses, locale) })}</small>}
                  </div>;
                })}
                <div className="gateway-route-controls">
                  <button type="button" onClick={() => move(index, -1)} disabled={index === 0} aria-label={t("route.move_up", { name: name })}>↑</button>
                  <button type="button" onClick={() => move(index, 1)} disabled={index === route.length - 1} aria-label={t("route.move_down", { name: name })}>↓</button>
                  <button type="button" onClick={() => setRoute((current) => current.filter((_, itemIndex) => itemIndex !== index))} aria-label={t("route.remove_from_route", { name: name })}>{t("route.remove")}</button>
                </div>
                {host ? <label>
                  {t("route.method_label", { name })}
                  <select value={step.method_id} onChange={(event) => setRoute((current) => current.map((item, itemIndex) =>
                    itemIndex === index ? { ...step, method_id: event.target.value } : item))}>
                    {!method ? <option value={step.method_id}>{t("route.missing_method")}</option> : null}
                    {host.connection_methods.map((candidate) => <option key={candidate.method_id} value={candidate.method_id}>{candidate.name}</option>)}
                  </select>
                </label> : null}
                <label>
                  {t("route.next_connection")}
                  <select value={step.mode} onChange={(event) => setRoute((current) => current.map((item, itemIndex) =>
                    itemIndex === index ? { ...step, mode: event.target.value as SshGatewayMode } : item))}>
                    <option value="automatic">{t("route.automatic_forward")}</option>
                    <option value="native_only">{t("route.ssh_forward")}</option>
                    <option value="agent_relay_only" disabled>{t("route.managed_relay")}</option>
                  </select>
                </label>
              </div>
            </div>;
          }
          const gateway = draftGateways.find(
            (item) => item.gateway_id === step.gateway_id,
          );
          if (!gateway) return <div className="gateway-route-step" key={step.gateway_id}>
            <div className="gateway-route-connector" aria-hidden="true">↓</div>
            <div className="gateway-route-card">
              <strong>{t("route.missing_gateway", { index: index + 1 })}</strong>
              <button type="button" onClick={() => setRoute((current) => current.filter((_, itemIndex) => itemIndex !== index))}>{t("route.remove_missing_gateway")}</button>
            </div>
          </div>;
          return (
            <div className="gateway-route-step" key={step.gateway_id}>
              <div className="gateway-route-connector" aria-hidden="true">↓</div>
              <div className="gateway-route-card">
                <div>
                  <strong>{index + 1}. {gateway.name}</strong>
                  <small>{endpointLabel(gateway)}</small>
                </div>
                <div className="gateway-route-controls">
                  <button type="button" onClick={() => move(index, -1)} disabled={index === 0} aria-label={t("route.move_up", { name: gateway.name })}>↑</button>
                  <button type="button" onClick={() => move(index, 1)} disabled={index === route.length - 1} aria-label={t("route.move_down", { name: gateway.name })}>↓</button>
                  <button
                    type="button"
                    onClick={() => editGateway(gateway)}
                    disabled={readonlyExisting && existingIds.has(gateway.gateway_id)}
                  >{t("route.edit")}</button>
                  <button
                    type="button"
                    onClick={() => setRoute((current) =>
                      current.filter((_, itemIndex) => itemIndex !== index))}
                    aria-label={t("route.remove_from_route", { name: gateway.name })}
                  >
                    {t("route.remove")}
                  </button>
                </div>
                {gateway.kind !== "socks5" ? <label>
                  {t("route.next_connection")}
                  <select
                    value={step.mode}
                    onChange={(event) => setRoute((current) =>
                      current.map((item) =>
                        "gateway_id" in item && item.gateway_id === step.gateway_id
                          ? { ...item, mode: event.target.value as SshGatewayMode }
                          : item))}
                  >
                    <option value="automatic">{t("route.automatic_forward")}</option>
                    <option value="native_only">{t("route.ssh_forward")}</option>
                    <option value="agent_relay_only" disabled>{t("route.managed_relay")}</option>
                  </select>
                </label> : <small>{t("route.socks_forward")}</small>}
              </div>
            </div>
          );
        })}
        <div className="gateway-route-connector" aria-hidden="true">↓</div>
        <RouteNode label={hostSetup?.address.trim() || target.destination} detail={t("route.destination")} />
      </div>

      {saved_hosts.length ? <section className="gateway-library" aria-labelledby="host-library-heading">
        <header><strong id="host-library-heading">{t("route.saved_hosts")}</strong></header>
        <p>{t("route.pick_saved_host")}</p>
        <div className="gateway-library-list gateway-host-library">
          {saved_hosts.map((host) => {
            const method_id = selectedHostMethod(host);
            const method = host.connection_methods.find((candidate) => candidate.method_id === method_id);
            const reason = routeHostIds.has(host.host_id) ? null
              : resolveRoute([...route, { host_id: host.host_id, method_id, mode: "automatic" }]).error;
            return <div key={host.host_id}>
              <span><strong>{host.name}</strong>{method ? <small>{endpointLabel({ ...method.target, gateway_id: host.host_id, name: host.name })}</small> : null}
                {reason ? <small role="status">{formatText(reason)}</small> : null}</span>
              <label>
                {t("route.method")}
                <select aria-label={t("route.host_method", { host: host.name })} value={method_id}
                  onChange={(event) => setHostMethods((current) => ({ ...current, [host.host_id]: event.target.value }))}>
                  {host.connection_methods.map((candidate) => <option key={candidate.method_id} value={candidate.method_id}>
                    {candidate.method_id === host.preferred_method_id ? t("route.preferred_method", { name: candidate.name }) : candidate.name}
                  </option>)}
                </select>
              </label>
              <button type="button" aria-label={t("route.add_hop", { host: host.name })} onClick={() => addHost(host)}
                disabled={routeHostIds.has(host.host_id) || Boolean(reason)}>{routeHostIds.has(host.host_id) ? t("route.added") : t("route.add_host")}</button>
            </div>;
          })}
        </div>
      </section> : null}

      <section className="gateway-library" aria-labelledby="gateway-library-heading">
        <header>
          <strong id="gateway-library-heading">{t("route.saved_gateways")}</strong>
          <button type="button" onClick={() => setEditing(emptyDraft())} disabled={route.length >= 8}>
            {t("route.new_gateway")}
          </button>
        </header>
        {draftGateways.length === 0 ? (
          <p>{t("route.no_saved_gateways")}</p>
        ) : (
          <div className="gateway-library-list">
            {draftGateways.map((gateway) => (
              <div key={gateway.gateway_id}>
                <span><strong>{gateway.name}</strong><small>{endpointLabel(gateway)}</small></span>
                <button
                  type="button"
                  onClick={() => editGateway(gateway)}
                  disabled={readonlyExisting && existingIds.has(gateway.gateway_id)}
                >{t("route.edit")}</button>
                <button
                  type="button"
                  onClick={() => deleteGateway(gateway)}
                  disabled={(readonlyExisting && existingIds.has(gateway.gateway_id)) ||
                    (otherUsage.get(gateway.gateway_id) ?? 0) > 0 || routeIds.has(gateway.gateway_id)}
                  title={(otherUsage.get(gateway.gateway_id) ?? 0) > 0 || routeIds.has(gateway.gateway_id)
                    ? t("route.remove_gateway_first")
                    : t("route.delete_gateway")}
                >
                  {t("route.delete")}
                </button>
                <button type="button" onClick={() => addExisting(gateway)} disabled={routeIds.has(gateway.gateway_id) || route.length >= 8}>
                  {routeIds.has(gateway.gateway_id) ? t("route.added") : t("route.add")}
                </button>
              </div>
            ))}
          </div>
        )}
      </section>
      {vpn_connections.length ? <section className="gateway-library" aria-labelledby="vpn-library-heading">
        <header><strong id="vpn-library-heading">{t("route.saved_vpns")}</strong></header>
        <div className="gateway-library-list">
          {vpn_connections.map((connection) => <div key={connection.connection_id}>
            <span><strong>{connection.name}</strong><small>{t("route.vpn_start_host")}</small></span>
            <button type="button" aria-label={t("route.add_vpn_route", { vpn: connection.name })}
              onClick={() => setRoute((current) => [...current, { vpn_connection_id: connection.connection_id }])}
              disabled={route.length >= 8 || !canAddVpn}>
              {t("route.add_vpn")}
            </button>
          </div>)}
        </div>
      </section> : null}

      {error && !missingVpn ? <InlineFeedback>{formatText(error)}</InlineFeedback> : null}
      <footer className="gateway-dialog-actions">
        <button type="button" onClick={onClose} disabled={saving}>{closeLabel ?? t("route.close")}</button>
        <button type="button" className="button-primary" onClick={() => void save()} disabled={saving || missingVpn || (requireGateway && route.length === 0)}>
          {saving ? (hostSetup ? t("route.connecting") : t("route.saving")) : (submitLabel ?? (hostSetup ? t("route.connect") : t("route.done")))}
        </button>
      </footer>
    </QuickInputFrame>
  );
}

function RouteNode({ label, detail }: { label: string; detail: string }) {
  return <div className="gateway-route-node"><strong>{label}</strong><small>{detail}</small></div>;
}

function RemoteVpnControls({ connection, owner, owner_label }: {
  connection: VpnConnection;
  owner: SshConnectionTarget;
  owner_label: string;
}) {
  const { t, locale, formatText } = useLocale();
  const [status, setStatus] = useState<VpnStatus | null>(null);
  const [checked, setChecked] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const [inventory_warnings, setInventoryWarnings] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<DisplayText | null>(null);
  async function perform(action: "status" | "stop" | "sign_in") {
    setBusy(true);
    setError(null);
    try {
      if (action === "stop") {
        setStatus(await stopVpn(status?.vpn_id ?? connection.connection_id, owner));
        setUnavailable(false);
      }
      else if (action === "sign_in") await openVpnSignIn(status?.vpn_id ?? connection.connection_id, owner);
      else {
        const snapshot = await vpnStatus(owner);
        const observed = snapshot.connections.find((item) => item.connection_id === connection.connection_id) ?? null;
        const warnings = snapshot.discovery_warnings ?? [];
        setStatus(observed);
        setInventoryWarnings(warnings);
        setUnavailable(!observed && warnings.length > 0);
        setChecked(true);
      }
    } catch (failure) {
      setError(localizedErrorText(failure));
      if (action !== "sign_in") {
        setUnavailable(true);
        setChecked(true);
      }
    } finally {
      setBusy(false);
    }
  }
  return <div className="gateway-remote-vpn">
    {checked ? <small>{t("route.vpn_owner_status", { owner: owner_label, status: unavailable || status?.status_unavailable
      ? t("route.status_unavailable") : status && status.state !== "stopped" ? vpnRouteDetail(connection, [status], locale) : t("route.vpn_stopped") })}</small> : null}
    {inventory_warnings.length ? <InlineFeedback severity="warning">{t("route.remote_inventory_partial", { detail: inventory_warnings.join(" ") })}</InlineFeedback> : null}
    <div className="gateway-route-controls">
      <button type="button" disabled={busy} onClick={() => void perform("status")}>{t("route.check_vpn")}</button>
      {status?.running ? <button type="button" disabled={busy || unavailable || status.status_unavailable} onClick={() => void perform("stop")}>{t("route.disconnect_vpn")}</button> : null}
      {vpnNeedsSignIn(status) ? <button type="button" disabled={busy || unavailable} onClick={() => void perform("sign_in")}>{t("route.sign_in_vpn")}</button> : null}
    </div>
    {error ? <InlineFeedback>{formatText(error)}</InlineFeedback> : null}
  </div>;
}

function GatewayForm({
  draft,
  onChange,
}: {
  draft: GatewayDraft;
  onChange(draft: GatewayDraft): void;
}) {
  const { t } = useLocale();
  const field = (key: keyof GatewayDraft) =>
    (event: React.ChangeEvent<HTMLInputElement>) =>
      onChange({ ...draft, [key]: event.target.value });
  return (
    <form className="gateway-form" onSubmit={(event) => event.preventDefault()}>
      <label>{t("route.type")}
        <select value={draft.kind} onChange={(event) => onChange({ ...draft, kind: event.target.value as GatewayDraft["kind"], hostname: "", identity_file: "", port: "" })}>
          <option value="ssh">SSH</option>
          <option value="socks5">SOCKS5</option>
        </select>
      </label>
      <label>{t("route.name")}<input value={draft.name} onChange={field("name")} placeholder={t("route.name_example")} /></label>
      <label>{draft.kind === "socks5" ? t("route.proxy_address") : t("route.ssh_destination")}<input value={draft.destination} onChange={field("destination")} placeholder="edge.example" /></label>
      {draft.kind === "ssh" ? <label>{t("route.override")}<input value={draft.hostname} onChange={field("hostname")} placeholder={t("route.optional")} /></label> : null}
      <label>{draft.kind === "socks5" ? t("route.optional_user") : t("route.user")}<input value={draft.user} onChange={field("user")} placeholder={draft.kind === "socks5" ? t("route.prompt_password") : t("route.from_config")} /></label>
      <label>{t("route.port")}<input inputMode="numeric" value={draft.port} onChange={field("port")} placeholder={draft.kind === "socks5" ? "1080" : "22"} /></label>
      <p className="quick-input-description">
        {draft.kind === "socks5"
          ? t("route.socks_hint")
          : t("route.ssh_hint")}
      </p>
    </form>
  );
}

function emptyDraft(): GatewayDraft {
  return {
    kind: "ssh",
    gateway_id: crypto.randomUUID(),
    name: "",
    destination: "",
    hostname: "",
    user: "",
    port: "",
    identity_file: "",
  };
}

function toDraft(gateway: WorkspaceSshGateway): GatewayDraft {
  return {
    kind: gateway.kind ?? "ssh",
    gateway_id: gateway.gateway_id,
    name: gateway.name,
    destination: gateway.destination,
    hostname: gateway.hostname ?? "",
    user: gateway.user ?? "",
    port: gateway.port?.toString() ?? "",
    identity_file: gateway.identity_file ?? "",
  };
}

function fromDraft(draft: GatewayDraft): WorkspaceSshGateway | null {
  const port = draft.port ? Number(draft.port) : undefined;
  const fields = [draft.name, draft.destination, draft.hostname, draft.user, draft.identity_file];
  if (
    !draft.name.trim() ||
    !draft.destination.trim() ||
    fields.some((value) => /[\x00-\x1f\x7f]/u.test(value)) ||
    /[,@]/u.test(draft.destination) ||
    /[,@]/u.test(draft.hostname) ||
    /[,@]/u.test(draft.user) ||
    (port !== undefined && (!Number.isInteger(port) || port < 1 || port > 65535)) ||
    (draft.kind === "socks5" && (port === undefined || !!draft.hostname.trim() || !!draft.identity_file.trim()))
  ) return null;
  return {
    gateway_id: draft.gateway_id,
    ...(draft.kind === "socks5" ? { kind: "socks5" as const } : {}),
    name: draft.name.trim(),
    destination: draft.destination.trim(),
    ...(draft.hostname.trim() ? { hostname: draft.hostname.trim() } : {}),
    ...(draft.user.trim() ? { user: draft.user.trim() } : {}),
    ...(port !== undefined ? { port } : {}),
    ...(draft.identity_file.trim() ? { identity_file: draft.identity_file.trim() } : {}),
  };
}

function endpointLabel(gateway: WorkspaceSshGateway): string {
  const host = gateway.hostname ?? gateway.destination;
  return `${gateway.kind === "socks5" ? "SOCKS5 · " : "SSH · "}${gateway.user ? `${gateway.user}@` : ""}${host}${gateway.port ? `:${gateway.port}` : ""}`;
}

function sameEndpoint(left: WorkspaceSshGateway, right: WorkspaceSshGateway): boolean {
  return (left.kind ?? "ssh") === (right.kind ?? "ssh") &&
    left.destination === right.destination &&
    left.hostname === right.hostname &&
    left.user === right.user &&
    left.port === right.port &&
    left.identity_file === right.identity_file;
}

function gatewayUsage(
  targets: readonly SshConnectionTarget[],
  gateways: readonly WorkspaceSshGateway[],
  hosts: readonly WorkspaceHost[],
): Map<string, number> {
  const users = new Map<string, Set<string>>();
  for (const target of targets) {
    let resolved = target.gateways ?? [];
    try { resolved = resolveSshGateways(target, gateways, hosts).gateways ?? []; }
    catch { /* Keep direct references and live snapshots for unavailable routes. */ }
    const gateway_ids = new Set([
      ...(target.gateway_route ?? []).flatMap((step) => "gateway_id" in step ? [step.gateway_id] : []),
      ...resolved.flatMap((gateway) => gateway.kind !== "vpn" ? [gateway.gateway_id] : []),
    ]);
    for (const gateway_id of gateway_ids) {
      const host_ids = users.get(gateway_id) ?? new Set<string>();
      host_ids.add(target.host_id ?? target.destination);
      users.set(gateway_id, host_ids);
    }
  }
  return new Map([...users].map(([gateway_id, host_ids]) => [gateway_id, host_ids.size]));
}
