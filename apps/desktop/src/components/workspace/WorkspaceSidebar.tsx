import { Icon } from "../ui/Icon";
import type { ReactNode } from "react";
import type { WorkspaceSidebarView } from "../../lib/types";

interface Props {
  selected: WorkspaceSidebarView;
  onSelect(view: WorkspaceSidebarView): void;
  sessions: ReactNode;
  tasks: ReactNode;
  ports: ReactNode;
  vpn: ReactNode;
  error: string | null;
  on_keybindings?(): void;
}

export function WorkspaceSidebar({
  selected,
  onSelect,
  sessions,
  tasks,
  ports,
  vpn,
  error,
  on_keybindings,
}: Props) {
  const views = ["sessions", "tasks", "ports", "vpn"] as const;
  const labels = {
    sessions: "Sessions",
    tasks: "Tasks",
    ports: "Ports",
    vpn: "VPN",
  } as const;
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
              title={labels[view]}
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
            </button>
          ))}
        </nav>
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
        {error ? (
          <p className="task-inline-error" role="alert">
            {error}
          </p>
        ) : null}
      </div>
    </div>
  );
}
