import { useState } from "react";
import type { VpnConnection, WorkspaceConnectionMethod, WorkspaceHost } from "../../lib/types";
import { QuickInputFrame } from "../commands/QuickInputFrame";
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
  const [draft, setDraft] = useState<WorkspaceHost>(() => ({
    ...host,
    connection_methods: host.connection_methods.map((method) => ({ ...method })),
  }));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
      setError(failure instanceof Error ? failure.message : String(failure));
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
      setError(failure instanceof Error ? failure.message : String(failure));
      setSaving(false);
    }
  }

  return (
    <QuickInputFrame
      title={`Host settings · ${host.name}`}
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
        <strong>Host settings</strong>
        <button type="button" onClick={close} disabled={saving}>Cancel</button>
      </header>
      <div className="host-settings-content">
        <label className="host-settings-name">
          Host name
          <input
            autoFocus
            value={draft.name}
            onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))}
            disabled={saving || unavailable}
          />
        </label>
        {unavailable ? <p className="host-settings-description">This host is unavailable. Restore its connection before editing, or remove it from the workspace below.</p> : null}
        <p className="host-settings-description">One machine and remote account. Choose how to reach it; sessions stay with this host.</p>
        {host.source === "ssh_config" ? <p className="host-settings-description">From SSH config. Customizing this host saves a copy in hosts.json; your SSH config stays unchanged.</p> : null}
        {host.source === "tailscale" ? <p className="host-settings-description">Discovered from Tailscale. Customizing this virtual host saves it in hosts.json.</p> : null}
        <div className="host-settings-section-heading">
          <strong>Connection methods</strong>
          <button type="button" onClick={onAddMethod} disabled={saving || dirty || unavailable}>
            <Icon name="plus" size={14} /> Add connection
          </button>
        </div>
        <p className="host-settings-description">Connect uses the preferred method. Choose another method explicitly when needed.</p>
        <div className="host-method-list">
          {draft.connection_methods.map((method) => {
            const preferred = method.method_id === draft.preferred_method_id;
            const steps = method.target.gateway_route?.length ?? 0;
            const vpn = vpn_connections.find((connection) => connection.connection_id === method.target.vpn_connection_id);
            const route = method.target.vpn_connection_id
              ? `Via VPN · ${vpn?.name ?? "Unavailable saved VPN"}`
              : steps ? `Via ${steps} route step${steps === 1 ? "" : "s"}` : "Direct SSH";
            return (
              <section className="host-method" key={method.method_id} aria-label={method.name}>
                <div className="host-method-heading">
                  <Icon name="plug" size={16} />
                  <label>
                    <span className="host-method-label">Connection name</span>
                    <input
                      aria-label={`Connection name for ${method.name}`}
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
                  {preferred ? <span className="host-method-preferred"><Icon name="check" size={12} /> Preferred</span> : null}
                </div>
                <p className="host-method-endpoint">{endpoint(method)} · {route}</p>
                {method.target.unavailable ? <p className="quick-input-error" role="status">{method.target.unavailable}</p> : null}
                <div className="host-method-actions">
                  <button type="button" disabled={saving || dirty || unavailable || Boolean(method.target.unavailable)} onClick={() => onConnect(method)}>Connect using</button>
                  <button type="button" disabled={saving || dirty || unavailable} onClick={() => onEditMethod(method)}>Edit connection</button>
                  <button type="button" disabled={saving || preferred || unavailable} onClick={() => setDraft((current) => ({ ...current, preferred_method_id: method.method_id }))}>Make preferred</button>
                  <button
                    type="button"
                    className="host-method-remove"
                    disabled={saving || unavailable || draft.connection_methods.length <= 1}
                    title={draft.connection_methods.length <= 1 ? "Keep at least one connection method." : "Remove this saved connection method."}
                    onClick={() => removeMethod(method.method_id)}
                  >Remove</button>
                </div>
              </section>
            );
          })}
        </div>
        {dirty ? <p className="host-settings-description" role="status">Save or cancel these changes before adding, editing, or connecting through a method.</p> : null}
        {removable ? <section className="host-settings-danger-zone" aria-label="Danger zone">
          <strong>Danger zone</strong>
          <p className="host-settings-description">Remove this host, its workspace session entries, tabs, port forwards, and saved SSH credentials. Remote shells keep running.</p>
          {confirm_remove ? <>
            <p>Remove {host.name} from this workspace?</p>
            <button type="button" disabled={saving} onClick={() => setConfirmRemove(false)}>Keep host</button>
            <button type="button" className="host-settings-remove" disabled={saving} onClick={() => void remove()}>
              {saving ? "Removing…" : "Confirm remove host"}
            </button>
          </> : <button type="button" className="host-settings-remove" disabled={saving} onClick={() => setConfirmRemove(true)}>Remove host…</button>}
        </section> : null}
        {error ? <p className="quick-input-error" role="alert">{error}</p> : null}
      </div>
      <footer className="host-settings-actions">
        {on_update_components ? <button type="button" onClick={on_update_components} disabled={saving || dirty || unavailable}>Update components…</button> : null}
        <button type="button" onClick={close} disabled={saving}>Cancel</button>
        <button type="button" className="button-primary" onClick={() => void save()} disabled={saving || unavailable || !dirty || !valid}>
          {saving ? "Saving…" : "Save changes"}
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
