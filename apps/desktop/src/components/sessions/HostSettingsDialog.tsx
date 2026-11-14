import { localizedErrorText } from "../../lib/errors";
import { type DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useState } from "react";
import type { VpnConnection, WorkspaceConnectionMethod, WorkspaceHost } from "../../lib/types";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { InlineFeedback } from "../feedback/InlineFeedback";
import { Icon } from "../ui/Icon";
import "./hostSettings.css";

interface Props {
  host: WorkspaceHost;
  vpn_connections?: readonly VpnConnection[];
  onSave(host: WorkspaceHost): Promise<void>;
  onAddMethod(): void;
  onEditMethod(method: WorkspaceConnectionMethod): void;
  onConnect(method: WorkspaceConnectionMethod): void;
  on_update_components?(): void;
  on_remove?(): Promise<void>;
  onClose(): void;
}

export function HostSettingsDialog({ host, vpn_connections = [], onSave, onAddMethod, onEditMethod, onConnect, on_update_components, on_remove, onClose }: Props) {
  const { t, formatText } = useLocale();
  const [draft, setDraft] = useState<WorkspaceHost>(() => ({
    ...host,
    connection_methods: host.connection_methods.map((method) => ({ ...method })),
  }));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<DisplayText | null>(null);
  const [confirm_remove, setConfirmRemove] = useState(false);
  const unavailable = host.source === "unavailable";
  const removable = Boolean(on_remove) && host.host_id !== "local" && host.source !== "ssh_config" && host.source !== "tailscale";
  const dirty = JSON.stringify(draft) !== JSON.stringify(host);
  const valid = validName(draft.name) && draft.connection_methods.every((method) => validName(method.name));
  const close = () => { if (!saving) onClose(); };

  function removeMethod(method_id: string) {
    if (draft.connection_methods.length <= 1) return;
    setDraft((current) => {
      const connection_methods = current.connection_methods.filter((method) => method.method_id !== method_id);
      return {
        ...current,
        connection_methods,
        preferred_method_id: current.preferred_method_id === method_id
          ? connection_methods[0].method_id
          : current.preferred_method_id,
      };
    });
  }

  async function remove() {
    if (!removable || !on_remove || saving) return;
    setSaving(true);
    setError(null);
    try {
      await on_remove();
      onClose();
    } catch (failure) {
      setError(localizedErrorText(failure));
      setSaving(false);
    }
  }

  async function save() {
    if (!valid || saving) return;
    setSaving(true);
    setError(null);
    try {
      await onSave({
        ...draft,
        name: draft.name.trim(),
        connection_methods: draft.connection_methods.map((method) => ({ ...method, name: method.name.trim() })),
      });
      onClose();
    } catch (failure) {
      setError(localizedErrorText(failure));
      setSaving(false);
    }
  }

  return (
    <QuickInputFrame
      title={t("host_settings.for_host", { host: host.name })}
      className="host-settings-dialog"
      onDismiss={close}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          close();
        }
      }}
    >
      <header className="quick-input-heading">
        <strong>{t("host_settings.title")}</strong>
        <button type="button" onClick={close} disabled={saving}>{t("host_settings.cancel")}</button>
      </header>
      <div className="host-settings-content">
        <label className="host-settings-name">
          {t("host_settings.name")}
          <input
            autoFocus
            value={draft.name}
            onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))}
            disabled={saving || unavailable}
          />
        </label>
        {unavailable ? <p className="host-settings-description">{t("host_settings.unavailable")}</p> : null}
        <p className="host-settings-description">{t("host_settings.description")}</p>
        {host.source === "ssh_config" ? <p className="host-settings-description">{t("host_settings.ssh_config_description")}</p> : null}
        {host.source === "tailscale" ? <p className="host-settings-description">{t("host_settings.tailscale_description")}</p> : null}
        <div className="host-settings-section-heading">
          <strong>{t("host_settings.methods")}</strong>
          <button type="button" onClick={onAddMethod} disabled={saving || dirty || unavailable}>
            <Icon name="plus" size={14} /> {t("host_settings.add_method")}
          </button>
        </div>
        <p className="host-settings-description">{t("host_settings.preferred_description")}</p>
        <div className="host-method-list">
          {draft.connection_methods.map((method) => {
            const preferred = method.method_id === draft.preferred_method_id;
            const steps = method.target.gateway_route?.length ?? 0;
            const vpn = vpn_connections.find((connection) => connection.connection_id === method.target.vpn_connection_id);
            const route = method.target.vpn_connection_id
              ? t("host_settings.vpn", { vpn: vpn?.name ?? t("host_settings.unavailable_vpn") })
              : steps ? t("host_settings.steps", { count: steps }) : t("host_settings.direct");
            return (
              <section className="host-method" key={method.method_id} aria-label={method.name}>
                <div className="host-method-heading">
                  <Icon name="plug" size={16} />
                  <label>
                    <span className="host-method-label">{t("host_settings.method_name")}</span>
                    <input
                      aria-label={t("host_settings.method_name_for", { method: method.name })}
                      value={method.name}
                      disabled={saving || unavailable}
                      onChange={(event) => setDraft((current) => ({
                        ...current,
                        connection_methods: current.connection_methods.map((item) => item.method_id === method.method_id
                          ? { ...item, name: event.target.value }
                          : item),
                      }))}
                    />
                  </label>
                  {preferred ? <span className="host-method-preferred"><Icon name="check" size={12} /> {t("host_settings.preferred")}</span> : null}
                </div>
                <p className="host-method-endpoint">{endpoint(method)} · {route}</p>
                {method.target.unavailable ? <p className="quick-input-error" role="status">{method.target.unavailable}</p> : null}
                <div className="host-method-actions">
                  <button type="button" disabled={saving || dirty || unavailable || Boolean(method.target.unavailable)} onClick={() => onConnect(method)}>{t("host_settings.connect_using")}</button>
                  <button type="button" disabled={saving || dirty || unavailable} onClick={() => onEditMethod(method)}>{t("host_settings.edit_method")}</button>
                  <button type="button" disabled={saving || preferred || unavailable} onClick={() => setDraft((current) => ({ ...current, preferred_method_id: method.method_id }))}>{t("host_settings.make_preferred")}</button>
                  <button
                    type="button"
                    className="host-method-remove"
                    disabled={saving || unavailable || draft.connection_methods.length <= 1}
                    title={draft.connection_methods.length <= 1 ? t("host_settings.keep_one") : t("host_settings.remove_method_hint")}
                    onClick={() => removeMethod(method.method_id)}
                  >{t("host_settings.remove")}</button>
                </div>
              </section>
            );
          })}
        </div>
        {dirty ? <p className="host-settings-description" role="status">{t("host_settings.save_before_connect")}</p> : null}
        {removable ? <section className="host-settings-danger-zone" aria-label={t("host_settings.danger_zone")}>
          <strong>{t("host_settings.danger_zone")}</strong>
          <p className="host-settings-description">{t("host_settings.remove_description")}</p>
          {confirm_remove ? <>
            <p>{t("host_settings.confirm_description", { host: host.name })}</p>
            <button type="button" disabled={saving} onClick={() => setConfirmRemove(false)}>{t("host_settings.keep")}</button>
            <button type="button" className="host-settings-remove" disabled={saving} onClick={() => void remove()}>
              {saving ? t("host_settings.removing") : t("host_settings.confirm_remove")}
            </button>
          </> : <button type="button" className="host-settings-remove" disabled={saving} onClick={() => setConfirmRemove(true)}>{t("host_settings.remove_host")}</button>}
        </section> : null}
        {error ? <InlineFeedback>{formatText(error)}</InlineFeedback> : null}
      </div>
      <footer className="host-settings-actions">
        {on_update_components ? <button type="button" onClick={on_update_components} disabled={saving || dirty || unavailable}>{t("host_settings.update_components")}</button> : null}
        <button type="button" onClick={close} disabled={saving}>{t("host_settings.cancel")}</button>
        <button type="button" className="button-primary" onClick={() => void save()} disabled={saving || unavailable || !dirty || !valid}>
          {saving ? t("host_settings.saving") : t("host_settings.save")}
        </button>
      </footer>
    </QuickInputFrame>
  );
}

function validName(value: string): boolean {
  const name = value.trim();
  return name.length > 0 && name.length <= 4096 && !/[\x00-\x1f\x7f-\x9f]/u.test(value);
}

function endpoint(method: WorkspaceConnectionMethod): string {
  const target = method.target;
  const hostname = target.hostname ?? target.destination;
  const address = hostname.includes(":") ? `[${hostname}]` : hostname;
  return `${target.user ? `${target.user}@` : ""}${address}${target.port ? `:${target.port}` : ""}`;
}
