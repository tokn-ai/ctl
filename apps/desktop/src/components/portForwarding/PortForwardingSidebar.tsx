import type {
  PortForwardStatus,
  SshConnectionTarget,
  WorkspacePortForward,
} from "../../lib/types";
import { targetLabel } from "../../features/targets/targets";
import { Icon } from "../ui/Icon";
import "./portForwarding.css";

interface Props {
  targets: readonly SshConnectionTarget[];
  forwards: readonly WorkspacePortForward[];
  statuses: ReadonlyMap<string, PortForwardStatus>;
  busy: ReadonlySet<string>;
  hostErrors: ReadonlyMap<string, string>;
  refreshing: boolean;
  lastRefreshedAt: number | null;
  onRefresh(): void;
  onSetEnabled(
    target: SshConnectionTarget,
    forward: WorkspacePortForward,
    enabled: boolean,
  ): void;
  onManage(target: SshConnectionTarget): void;
}

export function PortForwardingSidebar({
  targets,
  forwards,
  statuses,
  busy,
  hostErrors,
  refreshing,
  lastRefreshedAt,
  onRefresh,
  onSetEnabled,
  onManage,
}: Props) {
  const groups = targets
    .map((target) => ({
      target,
      forwards: forwards
        .filter((forward) => forward.host_id === target.host_id)
        .sort((left, right) =>
          left.local_port - right.local_port || left.name.localeCompare(right.name),
        ),
    }))
    .filter((group) => group.forwards.length > 0);

  return (
    <aside className="port-sidebar" aria-label="Port forwarding">
      <header className="sidebar-header">
        <strong>Port forwarding</strong>
        <span className="port-sidebar-count">{forwards.length}</span>
        <button
          className="icon-button"
          type="button"
          onClick={onRefresh}
          disabled={refreshing}
          aria-label="Refresh port forwarding"
          title="Refresh port forwarding"
        >
          <Icon name="refresh" class_name={refreshing ? "port-refreshing" : undefined} />
        </button>
      </header>

      <div className="port-sidebar-list">
        {groups.length === 0 ? (
          <div className="sidebar-state">
            <Icon name="ports" size={28} class_name="empty-glyph" />
            <p>No saved port forwards.</p>
            <small>Use a host’s forwarding action to add one.</small>
          </div>
        ) : null}
        {groups.map(({ target, forwards: hostForwards }) => (
          <section
            className="port-sidebar-group"
            key={target.host_id}
            aria-label={`${targetLabel(target)} port forwards`}
          >
            <header>
              <h3>{targetLabel(target)} <span>{hostForwards.length}</span></h3>
              <button type="button" onClick={() => onManage(target)}>Manage</button>
            </header>
            {hostErrors.get(target.host_id!) ? (
              <p className="port-sidebar-error" role="status">
                {hostErrors.get(target.host_id!)}
              </p>
            ) : null}
            {hostForwards.map((forward) => {
              const changing = busy.has(forward.forward_id);
              const status = forwardingStatus(forward, statuses.get(forward.forward_id));
              return (
                <div className="port-sidebar-row" key={forward.forward_id}>
                  <button
                    className="port-sidebar-details"
                    type="button"
                    onClick={() => onManage(target)}
                    title={`Manage ${forward.name}`}
                  >
                    <span className={`port-sidebar-indicator ${status.tone}`} aria-hidden="true" />
                    <span>
                      <strong>{forward.name}</strong>
                      <code>{forward.bind_address}:{forward.local_port}</code>
                      <small>→ {forward.remote_host}:{forward.remote_port}</small>
                      <small className={`port-sidebar-status ${status.tone}`}>
                        {changing ? "Changing…" : status.label}
                      </small>
                    </span>
                  </button>
                  <button
                    className="port-sidebar-toggle"
                    type="button"
                    disabled={changing}
                    onClick={() => onSetEnabled(target, forward, !forward.enabled)}
                  >
                    {forward.enabled ? "Stop" : "Start"}
                  </button>
                </div>
              );
            })}
          </section>
        ))}
      </div>

      <footer className="port-sidebar-footer">
        <small>
          {lastRefreshedAt === null
            ? "Runtime status has not been checked."
            : `Updated ${new Date(lastRefreshedAt).toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
              })}`}
        </small>
      </footer>
    </aside>
  );
}

function forwardingStatus(
  forward: WorkspacePortForward,
  status: PortForwardStatus | undefined,
): { label: string; tone: "active" | "waiting" | "error" | "stopped" } {
  if (!forward.enabled) return { label: "Stopped", tone: "stopped" };
  if (!status) return { label: "Checking…", tone: "waiting" };
  switch (status.state) {
    case "active":
      return { label: "Active", tone: "active" };
    case "error":
      return { label: status.message ?? "Error", tone: "error" };
    case "waiting_for_authentication":
      return { label: status.message ?? "Waiting for SSH", tone: "waiting" };
  }
}
