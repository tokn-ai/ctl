import type { SessionSummary } from "../../lib/types";

interface TerminalTabsProps {
  tabs: readonly SessionSummary[];
  activeSessionId: string | null;
  canCreate: boolean;
  onSelect(session: SessionSummary): void;
  onClose(session: SessionSummary): void;
  onCreate(): void;
}

export function TerminalTabs({
  tabs,
  activeSessionId,
  canCreate,
  onSelect,
  onClose,
  onCreate,
}: TerminalTabsProps) {
  return (
    <nav className="terminal-tabs" aria-label="Terminal tabs">
      <div className="terminal-tab-list" role="tablist">
        {tabs.map((tab) => {
          const active = tab.session_id === activeSessionId;
          return (
            <div
              className={`terminal-tab ${active ? "active" : ""}`}
              key={tab.session_id}
            >
              <button
                className="terminal-tab-select"
                type="button"
                role="tab"
                aria-selected={active}
                title={tab.name}
                onClick={() => onSelect(tab)}
              >
                <span className="terminal-tab-dot" aria-hidden="true" />
                <span>{tab.name}</span>
              </button>
              <button
                className="terminal-tab-close"
                type="button"
                aria-label={`Close ${tab.name} tab`}
                title="Close tab; keep session running"
                onClick={() => onClose(tab)}
              >
                ×
              </button>
            </div>
          );
        })}
      </div>
      <button
        className="terminal-tab-new"
        type="button"
        aria-label="New tab in current folder"
        title="New tab in current folder"
        disabled={!canCreate}
        onClick={onCreate}
      >
        +
      </button>
    </nav>
  );
}
