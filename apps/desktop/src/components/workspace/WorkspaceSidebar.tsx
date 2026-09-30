import { Icon } from "../ui/Icon";
import type { ReactNode } from "react";
import type { VpnState, WorkspaceSidebarView } from "../../lib/types";

interface Props {
  selected: WorkspaceSidebarView;
  onSelect(view: WorkspaceSidebarView): void;
  sessions: ReactNode;
  tasks: ReactNode;
  ports: ReactNode;
  vpn: ReactNode;
  vpn_state?: VpnState;
  vpn_active_count?: number;
  vpn_status_stale?: boolean;
  on_keybindings?(): void;
  on_about?(): void;
  about_open?: boolean;
  on_credentials?(): void;
  credentials_open?: boolean;
}

export function WorkspaceSidebar({
  selected,
  onSelect,
  sessions,
  tasks,
  ports,
  vpn,
  vpn_state,
  vpn_active_count = 0,
  vpn_status_stale = false,
  on_keybindings,
  on_about,
  about_open = false,
  on_credentials,
  credentials_open = false,
}: Props) {
  const views = ["sessions", "tasks", "ports", "vpn"] as const;
  const labels = {
    sessions: "Sessions",
    tasks: "Tasks",
    ports: "Ports",
    vpn: "VPN",
  } as const;
  const vpn_state_description = vpn_status_stale ? "Status unavailable" : vpn_state ? {
    starting: "Connecting",
    connected: "Connected",
    stopping: "Disconnecting",
    stopped: "Disconnected",
  }[vpn_state] : undefined;
  const vpn_description = !vpn_status_stale && vpn_active_count > 1 ? `${vpn_active_count} active VPNs` : vpn_state_description;
  return (
    <div className="workspace-sidebar">
      <div className="sidebar-rail">
        <div className="workbench-brand" title="rmux" aria-label="rmux">
          <Icon name="terminal" size={24} />
        </div>
        <nav
          className="sidebar-activity"
          role="tablist"
          aria-label="Sidebar"
          aria-orientation="vertical"
        >
          {views.map((view, index) => (
            <button
              key={view}
              id={`sidebar-tab-${view}`}
              role="tab"
              aria-label={labels[view]}
              aria-description={view === "vpn" ? vpn_description : undefined}
              title={view === "vpn" && vpn_description ? `VPN — ${vpn_description}` : labels[view]}
              aria-selected={selected === view}
              aria-controls={`sidebar-panel-${view}`}
              tabIndex={selected === view ? 0 : -1}
              onClick={() => onSelect(view)}
              onKeyDown={(event) => {
                if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key))
                  return;
                event.preventDefault();
                const next =
                  event.key === "Home"
                    ? views[0]
                    : event.key === "End"
                      ? views[views.length - 1]
                      : event.key === "ArrowUp"
                        ? views[(index - 1 + views.length) % views.length]
                        : views[(index + 1) % views.length];
                onSelect(next);
                document.getElementById(`sidebar-tab-${next}`)?.focus();
              }}
            >
              <Icon name={view === "sessions" ? "terminal" : view} size={23} />
              {view === "vpn" && (vpn_status_stale || (vpn_state && vpn_state !== "stopped")) ? (
                <span
                  className={`vpn-activity-indicator ${vpn_status_stale ? "stale" : vpn_state}`}
                  aria-hidden="true"
                />
              ) : null}
            </button>
          ))}
        </nav>
        {on_credentials ? (
          <button className="rail-settings" type="button" onClick={on_credentials} aria-label="Credentials" title="Credentials" aria-pressed={credentials_open}>
            <Icon name="key" size={21} />
          </button>
        ) : null}
        {on_keybindings ? (
          <button
            className="rail-settings"
            type="button"
            onClick={on_keybindings}
            aria-label="Configure Keyboard Shortcuts"
            title="Keyboard Shortcuts"
          >
            <Icon name="keyboard" size={21} />
          </button>
        ) : null}
        {on_about ? (
          <button className="rail-settings" type="button" onClick={on_about} aria-label="About rmux" title="About rmux" aria-pressed={about_open}>
            <Icon name="info" size={21} />
          </button>
        ) : null}
      </div>
      <div className="sidebar-content">
        {views.map((view) => (
          <div
            key={view}
            id={`sidebar-panel-${view}`}
            role="tabpanel"
            aria-labelledby={`sidebar-tab-${view}`}
            hidden={selected !== view}
          >
            {{ sessions, tasks, ports, vpn }[view]}
          </div>
        ))}

      </div>
    </div>
  );
}
