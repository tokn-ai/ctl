import type { ReactNode } from "react";

type SidebarView = "sessions" | "tasks";
interface Props {
  selected: SidebarView;
  onSelect(view: SidebarView): void;
  sessions: ReactNode;
  tasks: ReactNode;
  error: string | null;
}

export function WorkspaceSidebar({
  selected,
  onSelect,
  sessions,
  tasks,
  error,
}: Props) {
  const views = ["sessions", "tasks"] as const;
  return (
    <div className="workspace-sidebar">
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
            aria-label={view === "sessions" ? "Sessions" : "Tasks"}
            title={view === "sessions" ? "Sessions" : "Tasks"}
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
                    ? views[1]
                    : views[(index + 1) % views.length];
              onSelect(next);
              document.getElementById(`sidebar-tab-${next}`)?.focus();
            }}
          >
            <svg
              width="22"
              height="22"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              aria-hidden="true"
            >
              {view === "sessions" ? (
                <>
                  <rect x="3" y="4" width="18" height="16" rx="2" />
                  <path d="m7 9 3 3-3 3m6 0h4" />
                </>
              ) : (
                <>
                  <rect x="4" y="3" width="16" height="18" rx="2" />
                  <path d="m7 8 1.5 1.5L11 7m2 2h4m-10 5 1.5 1.5L11 13m2 2h4" />
                </>
              )}
            </svg>
          </button>
        ))}
      </nav>
      <div className="sidebar-content">
        <div
          id="sidebar-panel-sessions"
          role="tabpanel"
          aria-labelledby="sidebar-tab-sessions"
          hidden={selected !== "sessions"}
        >
          {sessions}
        </div>
        <div
          id="sidebar-panel-tasks"
          role="tabpanel"
          aria-labelledby="sidebar-tab-tasks"
          hidden={selected !== "tasks"}
        >
          {tasks}
        </div>
        {error ? (
          <p className="task-inline-error" role="alert">
            {error}
          </p>
        ) : null}
      </div>
    </div>
  );
}
