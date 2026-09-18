import { useMemo, useState } from "react";
import type {
  SshConnectionTarget,
  SshGatewayMode,
  SshGatewayRouteStep,
  WorkspaceSshGateway,
} from "../../lib/types";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import "./gatewayRoute.css";

interface Props {
  target: SshConnectionTarget;
  gateways: readonly WorkspaceSshGateway[];
  targets: readonly SshConnectionTarget[];
  hostSetup?: {
    address: string;
    alias: string;
    identity_file: string;
    suggestions: readonly string[];
    warning: string | null;
    onAddressChange(value: string): void;
    onAliasChange(value: string): void;
    onIdentityFileChange(value: string): void;
  };
  readonlyExisting?: boolean;
  readonlyGatewayIds?: readonly string[];
  requireGateway?: boolean;
  closeLabel?: string;
  onSave(
    gateways: WorkspaceSshGateway[],
    route: SshGatewayRouteStep[],
  ): Promise<void>;
  onClose(): void;
}

interface GatewayDraft {
  gateway_id: string;
  name: string;
  destination: string;
  hostname: string;
  user: string;
  port: string;
  identity_file: string;
}

export function GatewayRouteDialog({
  target,
  gateways,
  targets,
  hostSetup,
  readonlyExisting = false,
  readonlyGatewayIds,
  requireGateway = false,
  closeLabel = "Close",
  onSave,
  onClose,
}: Props) {
  const [draftGateways, setDraftGateways] = useState<WorkspaceSshGateway[]>(
    () => gateways.map((gateway) => ({ ...gateway })),
  );
  const [route, setRoute] = useState<SshGatewayRouteStep[]>(() => [
    ...(target.gateway_route ?? []),
  ]);
  const [editing, setEditing] = useState<GatewayDraft | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const usage = useMemo(() => gatewayUsage(targets), [targets]);
  const otherUsage = useMemo(
    () => gatewayUsage(targets.filter((item) => item.host_id !== target.host_id)),
    [target.host_id, targets],
  );
  const routeIds = new Set(route.map((step) => step.gateway_id));
  const existingIds = new Set(
    readonlyGatewayIds ?? gateways.map((gateway) => gateway.gateway_id),
  );

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
      setError("Enter a name, SSH destination, and a valid port from 1 to 65535.");
      return;
    }
    const duplicate = draftGateways.some(
      (item) =>
        item.gateway_id !== gateway.gateway_id &&
        item.name.toLocaleLowerCase() === gateway.name.toLocaleLowerCase(),
    );
    if (duplicate) {
      setError("Gateway names must be unique.");
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
    if (requireGateway && route.length === 0) {
      setError("Add at least one gateway to this route.");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onSave(draftGateways, route);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      setSaving(false);
    }
  }

  if (editing) {
    const sharedCount = usage.get(editing.gateway_id) ?? 0;
    return (
      <QuickInputFrame
        title="Edit SSH gateway"
        onDismiss={() => setEditing(null)}
        className="gateway-route-dialog"
      >
        <header className="quick-input-heading">
          <strong>{sharedCount > 1 ? "Edit shared gateway" : "SSH gateway"}</strong>
          <button type="button" onClick={() => setEditing(null)}>Back</button>
        </header>
        {sharedCount > 1 ? (
          <p className="gateway-shared-warning" role="status">
            This gateway is used by {sharedCount} hosts. Saving changes updates every route that references it.
          </p>
        ) : null}
        <GatewayForm draft={editing} onChange={setEditing} />
        {error ? <p className="quick-input-error" role="alert">{error}</p> : null}
        <footer className="gateway-dialog-actions">
          <button type="button" onClick={() => setEditing(null)}>Cancel</button>
          <button type="button" className="button-primary" onClick={saveGateway}>
            Save gateway
          </button>
        </footer>
      </QuickInputFrame>
    );
  }

  return (
    <QuickInputFrame
      title={hostSetup ? "Add host with gateways" : "Connection route"}
      onDismiss={onClose}
      className="gateway-route-dialog"
    >
      <header className="quick-input-heading">
        <strong>{hostSetup ? "Add host with gateways" : `Connection route · ${target.destination}`}</strong>
        <button type="button" onClick={onClose} disabled={saving}>{closeLabel}</button>
      </header>
      {hostSetup ? (
        <section className="gateway-host-form" aria-label="Host details">
          <label>
            SSH host or config alias
            <input
              autoFocus
              list="routed-host-suggestions"
              value={hostSetup.address}
              onChange={(event) => hostSetup.onAddressChange(event.target.value)}
              placeholder="operator@server.internal:2222"
            />
          </label>
          <datalist id="routed-host-suggestions">
            {hostSetup.suggestions.map((suggestion) => (
              <option key={suggestion} value={suggestion} />
            ))}
          </datalist>
          <label>
            Name / SSH alias (optional)
            <input
              value={hostSetup.alias}
              onChange={(event) => hostSetup.onAliasChange(event.target.value)}
              placeholder="Defaults to the SSH host"
            />
          </label>
          <label>
            Identity file (optional)
            <input
              value={hostSetup.identity_file}
              onChange={(event) => hostSetup.onIdentityFileChange(event.target.value)}
              placeholder="~/.ssh/id_ed25519"
            />
          </label>
          {hostSetup.warning ? <p role="status">{hostSetup.warning}</p> : null}
        </section>
      ) : null}
      <p className="quick-input-description">
        Gateways are tried in order. Automatic mode currently uses native SSH forwarding. Managed agent relay fallback is the next runtime phase.
      </p>

      <div className="gateway-route-path">
        <RouteNode label="This Mac" detail="Direct SSH" />
        {route.map((step, index) => {
          const gateway = draftGateways.find(
            (item) => item.gateway_id === step.gateway_id,
          );
          if (!gateway) return null;
          return (
            <div className="gateway-route-step" key={step.gateway_id}>
              <div className="gateway-route-connector" aria-hidden="true">↓</div>
              <div className="gateway-route-card">
                <div>
                  <strong>{index + 1}. {gateway.name}</strong>
                  <small>{endpointLabel(gateway)}</small>
                </div>
                <div className="gateway-route-controls">
                  <button type="button" onClick={() => move(index, -1)} disabled={index === 0} aria-label={`Move ${gateway.name} up`}>↑</button>
                  <button type="button" onClick={() => move(index, 1)} disabled={index === route.length - 1} aria-label={`Move ${gateway.name} down`}>↓</button>
                  <button
                    type="button"
                    onClick={() => editGateway(gateway)}
                    disabled={readonlyExisting && existingIds.has(gateway.gateway_id)}
                  >Edit</button>
                  <button
                    type="button"
                    onClick={() => setRoute((current) =>
                      current.filter((item) => item.gateway_id !== step.gateway_id))}
                    aria-label={`Remove ${gateway.name} from route`}
                  >
                    Remove
                  </button>
                </div>
                <label>
                  Connection to next host
                  <select
                    value={step.mode}
                    onChange={(event) => setRoute((current) =>
                      current.map((item) =>
                        item.gateway_id === step.gateway_id
                          ? { ...item, mode: event.target.value as SshGatewayMode }
                          : item))}
                  >
                    <option value="automatic">Automatic · native forwarding</option>
                    <option value="native_only">Native SSH forwarding only</option>
                    <option value="agent_relay_only" disabled>Managed agent relay only · coming next</option>
                  </select>
                </label>
              </div>
            </div>
          );
        })}
        <div className="gateway-route-connector" aria-hidden="true">↓</div>
        <RouteNode label={hostSetup?.address.trim() || target.destination} detail="Destination" />
      </div>

      <section className="gateway-library" aria-labelledby="gateway-library-heading">
        <header>
          <strong id="gateway-library-heading">Saved gateways</strong>
          <button type="button" onClick={() => setEditing(emptyDraft())} disabled={route.length >= 8}>
            + New gateway
          </button>
        </header>
        {draftGateways.length === 0 ? (
          <p>No saved gateways. Add one to build this route.</p>
        ) : (
          <div className="gateway-library-list">
            {draftGateways.map((gateway) => (
              <div key={gateway.gateway_id}>
                <span><strong>{gateway.name}</strong><small>{endpointLabel(gateway)}</small></span>
                <button
                  type="button"
                  onClick={() => editGateway(gateway)}
                  disabled={readonlyExisting && existingIds.has(gateway.gateway_id)}
                >Edit</button>
                <button
                  type="button"
                  onClick={() => deleteGateway(gateway)}
                  disabled={(readonlyExisting && existingIds.has(gateway.gateway_id)) ||
                    (otherUsage.get(gateway.gateway_id) ?? 0) > 0 || routeIds.has(gateway.gateway_id)}
                  title={(otherUsage.get(gateway.gateway_id) ?? 0) > 0 || routeIds.has(gateway.gateway_id)
                    ? "Remove this gateway from every route before deleting it."
                    : "Delete saved gateway"}
                >
                  Delete
                </button>
                <button type="button" onClick={() => addExisting(gateway)} disabled={routeIds.has(gateway.gateway_id) || route.length >= 8}>
                  {routeIds.has(gateway.gateway_id) ? "Added" : "Add"}
                </button>
              </div>
            ))}
          </div>
        )}
      </section>

      {error ? <p className="quick-input-error" role="alert">{error}</p> : null}
      <footer className="gateway-dialog-actions">
        <button type="button" onClick={onClose} disabled={saving}>{closeLabel}</button>
        <button type="button" className="button-primary" onClick={() => void save()} disabled={saving || (requireGateway && route.length === 0)}>
          {saving ? (hostSetup ? "Connecting…" : "Saving…") : (hostSetup ? "Connect" : "Done")}
        </button>
      </footer>
    </QuickInputFrame>
  );
}

function RouteNode({ label, detail }: { label: string; detail: string }) {
  return <div className="gateway-route-node"><strong>{label}</strong><small>{detail}</small></div>;
}

function GatewayForm({
  draft,
  onChange,
}: {
  draft: GatewayDraft;
  onChange(draft: GatewayDraft): void;
}) {
  const field = (key: keyof GatewayDraft) =>
    (event: React.ChangeEvent<HTMLInputElement>) =>
      onChange({ ...draft, [key]: event.target.value });
  return (
    <form className="gateway-form" onSubmit={(event) => event.preventDefault()}>
      <label>Name<input value={draft.name} onChange={field("name")} placeholder="Office gateway" /></label>
      <label>SSH destination / alias<input value={draft.destination} onChange={field("destination")} placeholder="edge.example" /></label>
      <label>Hostname override<input value={draft.hostname} onChange={field("hostname")} placeholder="Optional" /></label>
      <label>User<input value={draft.user} onChange={field("user")} placeholder="From SSH config" /></label>
      <label>Port<input inputMode="numeric" value={draft.port} onChange={field("port")} placeholder="22" /></label>
      <p className="quick-input-description">
        Configure a gateway-specific identity file in your OpenSSH config. Per-gateway identity selection will be enabled with managed relay.
      </p>
    </form>
  );
}

function emptyDraft(): GatewayDraft {
  return {
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
    (port !== undefined && (!Number.isInteger(port) || port < 1 || port > 65535))
  ) return null;
  return {
    gateway_id: draft.gateway_id,
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
  return `${gateway.user ? `${gateway.user}@` : ""}${host}${gateway.port ? `:${gateway.port}` : ""}`;
}

function sameEndpoint(left: WorkspaceSshGateway, right: WorkspaceSshGateway): boolean {
  return left.destination === right.destination &&
    left.hostname === right.hostname &&
    left.user === right.user &&
    left.port === right.port &&
    left.identity_file === right.identity_file;
}

function gatewayUsage(targets: readonly SshConnectionTarget[]): Map<string, number> {
  const usage = new Map<string, number>();
  for (const target of targets) {
    for (const step of target.gateway_route ?? []) {
      usage.set(step.gateway_id, (usage.get(step.gateway_id) ?? 0) + 1);
    }
  }
  return usage;
}
