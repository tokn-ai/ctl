import type { SessionSummary, ShellStateSummary } from "../../lib/types";
import { formatTerminalTitle } from "../../features/tabs/terminalTitle";
import { sessionKey, targetLabel } from "../../features/targets/targets";

interface TerminalTabsProps {
  tabs: readonly SessionSummary[];
  shellStates: ReadonlyMap<string, ShellStateSummary>;
  activeSessionKey: string | null;
  canCreate: boolean;
  onSelect(session: SessionSummary): void;
  onClose(session: SessionSummary): void;
  onCreate(): void;
}

export function TerminalTabs({
  tabs,
  shellStates,
  activeSessionKey,
  canCreate,
  onSelect,
  onClose,
  onCreate,
}: TerminalTabsProps) {
  return (
    <nav className="terminal-tabs" aria-label="Terminal tabs">
      <div className="terminal-tab-list" role="tablist">
        {tabs.map((tab) => {
          const identity = sessionKey(tab);
          const active = identity === activeSessionKey;
          const title = formatTerminalTitle(
            tab,
            shellStates.get(identity) ?? null,
          );
          return (
            <div
              className={`terminal-tab ${active ? "active" : ""}`}
              key={identity}
            >
              <button
                className="terminal-tab-select"
                type="button"
                role="tab"
                aria-selected={active}
                aria-label={`${title.text} on ${targetLabel(tab.target)}`}
                title={`${title.text} · ${targetLabel(tab.target)}`}
                onClick={() => onSelect(tab)}
              >
                <span className="terminal-tab-dot" aria-hidden="true" />
                <span className="terminal-tab-host">{targetLabel(tab.target)}</span>
                <span className="terminal-tab-copy">
                  <span className="terminal-tab-path">
                    <bdi dir="ltr">{title.path}</bdi>
                  </span>
                  {title.command ? (
                    <>
                      <span className="terminal-tab-separator" aria-hidden="true">
                        —
                      </span>
                      <span className="terminal-tab-command">{title.command}</span>
                    </>
                  ) : null}
                </span>
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
