import { ControlPopover } from "../ui/ControlPopover";
import { useLocale } from "../../features/i18n/LocaleContext";
import type { LocalePreference } from "../../features/i18n/messages";
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
  const { t, preference, setPreference } = useLocale();
  const vpn_unavailable = vpn_status_stale || (vpn_inventory_incomplete && vpn_active_count === 0);
  const vpn_state_description = vpn_unavailable ? t("nav.status_unavailable") : vpn_state ? {
    starting: t("nav.vpn_connecting"),
    connected: t("nav.vpn_connected"),
    stopping: t("nav.vpn_disconnecting"),
    stopped: t("nav.vpn_disconnected"),
  }[vpn_state] : undefined;
  const vpn_activity_description = !vpn_unavailable && vpn_active_count > 1 ? t("nav.active_vpns", { v1: vpn_active_count }) : vpn_state_description;
  const vpn_description = vpn_inventory_incomplete && !vpn_unavailable && vpn_activity_description
    ? t("nav.incomplete_inventory", { v1: vpn_activity_description }) : vpn_activity_description;

  return (
    <aside hidden={hidden} className="workspace-sidebar" aria-label={t("nav.workspace")} id="workspace-sidebar">
      <div className="workspace-rail">
        <nav className="workspace-navigation" aria-label={t("nav.tools")}>
          <button type="button" title={t("nav.sessions_hint")} aria-label={t("command.category_sessions")} aria-pressed={selected === "sessions"} onClick={() => onSelect("sessions")}>
            <Icon name="terminal" size={20} />
          </button>
          <button type="button" id="workspace-tasks-toggle" aria-label={t("command.category_tasks")} title={running_task_count ? t("nav.tasks_running", { v1: running_task_count }) : t("command.category_tasks")} aria-pressed={selected === "tasks"} aria-controls="workspace-tasks" onClick={() => onSelect("tasks")}>
            <Icon name="tasks" size={20} />
            {running_task_count > 0 && <span className="rail-activity-dot" aria-label={t("nav.running_tasks", { v1: running_task_count })} />}
          </button>
          <button type="button" title={t("shell.port_forwarding")} aria-label={t("nav.ports")} aria-pressed={selected === "ports"} onClick={() => onSelect("ports")}>
            <Icon name="ports" size={20} />
          </button>
          <button type="button" aria-label="VPN" aria-pressed={selected === "vpn"} aria-description={vpn_description} title={vpn_description ? t("nav.vpn_hint", { v1: vpn_description }) : "VPN"} onClick={() => onSelect("vpn")}>
            <Icon name="vpn" size={20} />
            {vpn_unavailable || (vpn_state && vpn_state !== "stopped") ? (
              <span className={`vpn-activity-indicator ${vpn_unavailable ? "stale" : vpn_state}`} aria-hidden="true" />
            ) : null}
          </button>
        </nav>
        <div className="workspace-settings">
          <ControlPopover label={t("command.category_settings")} trigger_content={<Icon name="settings" size={20} />} placement="right" close_on_select>
            <label className="workspace-language">
              <span>{t("nav.language")}</span>
              <select value={preference} onChange={(event) => setPreference(event.currentTarget.value as LocalePreference)}>
                <option value="system">{t("nav.language_system")}</option>
                <option value="en">{t("nav.language_english")}</option>
                <option value="zh-Hans">{t("nav.language_chinese")}</option>
              </select>
            </label>
            {on_credentials && <button type="button" onClick={on_credentials} aria-label={t("command.category_credentials")} aria-pressed={credentials_open}><Icon name="key" />{t("command.category_credentials")}</button>}
            {on_keybindings && <button type="button" onClick={on_keybindings} aria-label={t("command.configure_shortcuts")}><Icon name="keyboard" />{t("shortcuts.title")}</button>}
            {on_about && <button type="button" onClick={on_about} aria-label={t("command.category_about")} aria-pressed={about_open}><Icon name="info" />{t("nav.about")}</button>}
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
