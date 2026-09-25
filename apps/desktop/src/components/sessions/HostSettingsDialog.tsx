import { useState } from "react";
import type { WorkspaceConnectionMethod, WorkspaceHost } from "../../lib/types";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { Icon } from "../ui/Icon";
import "./hostSettings.css";

interface Props {
  host: WorkspaceHost;
  onSave(host: WorkspaceHost): Promise<void>;
  onAddMethod(): void;
  onEditMethod(method: WorkspaceConnectionMethod): void;
  onConnect(method: WorkspaceConnectionMethod): void;
  onClose(): void;
}

export function HostSettingsDialog({ host, onSave, onAddMethod, onEditMethod, onConnect, onClose }: Props) {
  const [draft, setDraft] = useState<WorkspaceHost>(() => ({
    ...host,
    connection_methods: host.connection_methods.map((method) => ({ ...method })),
  }));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
            disabled={saving}
          />
        </label>
        <p className="host-settings-description">One machine and remote account. Choose how to reach it; sessions stay with this host.</p>
        {host.source === "ssh_config" ? <p className="host-settings-description">From SSH config. Customizing this host saves a copy in hosts.json; your SSH config stays unchanged.</p> : null}
        {host.source === "tailscale" ? <p className="host-settings-description">Discovered from Tailscale. Customizing this virtual host saves it in hosts.json.</p> : null}
        <div className="host-settings-section-heading">
          <strong>Connection methods</strong>
          <button type="button" onClick={onAddMethod} disabled={saving || dirty}>
            <Icon name="plus" size={14} /> Add connection
          </button>
        </div>
        <p className="host-settings-description">Connect uses the preferred method. Choose another method explicitly when needed.</p>
        <div className="host-method-list">
          {draft.connection_methods.map((method) => {
            const preferred = method.method_id === draft.preferred_method_id;
            const gateways = method.target.gateway_route?.length ?? 0;
            return (
              <section className="host-method" key={method.method_id} aria-label={method.name}>
                <div className="host-method-heading">
                  <Icon name="plug" size={16} />
                  <label>
                    <span className="host-method-label">Connection name</span>
                    <input
                      aria-label={`Connection name for ${method.name}`}
                      value={method.name}
                      disabled={saving}
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
                <p className="host-method-endpoint">{endpoint(method)} · {gateways ? `Via ${gateways} gateway${gateways === 1 ? "" : "s"}` : "Direct SSH"}</p>
                {method.target.unavailable ? <p className="quick-input-error" role="status">{method.target.unavailable}</p> : null}
                <div className="host-method-actions">
                  <button type="button" disabled={saving || dirty || Boolean(method.target.unavailable)} onClick={() => onConnect(method)}>Connect using</button>
                  <button type="button" disabled={saving || dirty} onClick={() => onEditMethod(method)}>Edit connection</button>
                  <button type="button" disabled={saving || preferred} onClick={() => setDraft((current) => ({ ...current, preferred_method_id: method.method_id }))}>Make preferred</button>
                  <button
                    type="button"
                    className="host-method-remove"
                    disabled={saving || draft.connection_methods.length <= 1}
                    title={draft.connection_methods.length <= 1 ? "Keep at least one connection method." : "Remove this saved connection method."}
                    onClick={() => removeMethod(method.method_id)}
                  >Remove</button>
                </div>
              </section>
            );
          })}
        </div>
        {dirty ? <p className="host-settings-description" role="status">Save or cancel these changes before adding, editing, or connecting through a method.</p> : null}
        {error ? <p className="quick-input-error" role="alert">{error}</p> : null}
      </div>
      <footer className="host-settings-actions">
        <button type="button" onClick={close} disabled={saving}>Cancel</button>
        <button type="button" className="button-primary" onClick={() => void save()} disabled={saving || !dirty || !valid}>
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
