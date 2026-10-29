import { ControlPopover } from "../ui/ControlPopover";
import { Icon } from "../ui/Icon";
import type { ReactNode } from "react";
import type { VpnState, WorkspaceSidebarView } from "../../lib/types";
import "./workspace.css";

interface Props {
  hidden?: boolean;
  selected: WorkspaceSidebarView;
  onSelect(view: WorkspaceSidebarView): void;
  sessions: ReactNode;
  tasks: ReactNode;
  ports: ReactNode;
  vpn: ReactNode;
  running_task_count?: number;
  vpn_state?: VpnState;
  vpn_active_count?: number;
  vpn_status_stale?: boolean;
  vpn_inventory_incomplete?: boolean;
  on_keybindings?(): void;
  on_about?(): void;
  about_open?: boolean;
  on_credentials?(): void;
  credentials_open?: boolean;
}

/** Each rail destination selects one sidebar without changing the active terminal. */
export function WorkspaceSidebar({
  hidden = false,
  selected,
  onSelect,
  sessions,
  tasks,
  ports,
  vpn,
  running_task_count = 0,
  vpn_state,
  vpn_active_count = 0,
  vpn_status_stale = false,
  vpn_inventory_incomplete = false,
  on_keybindings,
  on_about,
  about_open = false,
  on_credentials,
  credentials_open = false,
}: Props) {
  const vpn_unavailable = vpn_status_stale || (vpn_inventory_incomplete && vpn_active_count === 0);
  const vpn_state_description = vpn_unavailable ? "Status unavailable" : vpn_state ? {
    starting: "Connecting",
    connected: "Connected",
    stopping: "Disconnecting",
    stopped: "Disconnected",
  }[vpn_state] : undefined;
  const vpn_activity_description = !vpn_unavailable && vpn_active_count > 1 ? `${vpn_active_count} active VPNs` : vpn_state_description;
  const vpn_description = vpn_inventory_incomplete && !vpn_unavailable && vpn_activity_description
    ? `${vpn_activity_description} · Inventory incomplete` : vpn_activity_description;

  return (
    <aside hidden={hidden} className="workspace-sidebar" aria-label="Workspace navigation" id="workspace-sidebar">
      <div className="workspace-rail">
        <nav className="workspace-navigation" aria-label="Workspace tools">
          <button type="button" title="Hosts and sessions" aria-label="Sessions" aria-pressed={selected === "sessions"} onClick={() => onSelect("sessions")}>
            <Icon name="terminal" size={20} />
          </button>
          <button type="button" id="workspace-tasks-toggle" aria-label="Tasks" title={running_task_count ? `Tasks · ${running_task_count} running` : "Tasks"} aria-pressed={selected === "tasks"} aria-controls="workspace-tasks" onClick={() => onSelect("tasks")}>
            <Icon name="tasks" size={20} />
            {running_task_count > 0 && <span className="rail-activity-dot" aria-label={`${running_task_count} running tasks`} />}
          </button>
          <button type="button" title="Port forwarding" aria-label="Ports" aria-pressed={selected === "ports"} onClick={() => onSelect("ports")}>
            <Icon name="ports" size={20} />
          </button>
          <button type="button" aria-label="VPN" aria-pressed={selected === "vpn"} aria-description={vpn_description} title={vpn_description ? `VPN · ${vpn_description}` : "VPN"} onClick={() => onSelect("vpn")}>
            <Icon name="vpn" size={20} />
            {vpn_unavailable || (vpn_state && vpn_state !== "stopped") ? (
              <span className={`vpn-activity-indicator ${vpn_unavailable ? "stale" : vpn_state}`} aria-hidden="true" />
            ) : null}
          </button>
        </nav>
        <div className="workspace-settings">
          <ControlPopover label="Settings" trigger_content={<Icon name="settings" size={20} />} placement="right" close_on_select>
            {on_credentials && <button type="button" onClick={on_credentials} aria-label="Credentials" aria-pressed={credentials_open}><Icon name="key" />Credentials</button>}
            {on_keybindings && <button type="button" onClick={on_keybindings} aria-label="Configure Keyboard Shortcuts"><Icon name="keyboard" />Keyboard shortcuts</button>}
            {on_about && <button type="button" onClick={on_about} aria-label="About ctmux" aria-pressed={about_open}><Icon name="info" />About</button>}
          </ControlPopover>
        </div>
      </div>
      <div className="sidebar-content">
        <div hidden={selected !== "sessions"}>{sessions}</div>
        <div id="workspace-tasks" hidden={selected !== "tasks"}>{tasks}</div>
        <div hidden={selected !== "ports"}>{ports}</div>
        <div hidden={selected !== "vpn"}>{vpn}</div>
      </div>
    </aside>
  );
}
