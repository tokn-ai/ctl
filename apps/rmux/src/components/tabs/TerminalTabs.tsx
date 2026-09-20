import type { KeyboardEvent, ReactNode } from "react";
import type { SessionSummary, ShellStateSummary } from "../../lib/types";
import { formatTerminalTitle } from "../../features/tabs/terminalTitle";
import { sessionKey, targetLabel } from "../../features/targets/targets";
import { Icon } from "../ui/Icon";

export interface ExtraTab {
  tab_key: string;
  title: string;
  host: string;
  status: string;
}

interface TerminalTabsProps {
  extra_tabs?: ExtraTab[];
  tab_order?: string[];
  on_select_extra?(key: string): void;
  on_close_extra?(key: string): void;
  tabs: readonly SessionSummary[];
  shellStates: ReadonlyMap<string, ShellStateSummary>;
  activeSessionKey: string | null;
  canCreate: boolean;
  onSelect(session: SessionSummary): void;
  onClose(session: SessionSummary): void;
  onCreate(): void;
}

interface WorkspaceTabProps {
  active: boolean;
  title: string;
  host: string;
  close_label: string;
  close_hint: string;
  icon: ReactNode;
  children: ReactNode;
  on_select(): void;
  on_close(): void;
}

function WorkspaceTab({
  active, title, host, close_label, close_hint, icon, children,
  on_select, on_close,
}: WorkspaceTabProps) {
  return (
    <div className={`terminal-tab ${active ? "active" : ""}`}>
      <button
        className="terminal-tab-select"
        type="button"
        role="tab"
        aria-selected={active}
        aria-label={`${title} on ${host}`}
        title={`${title} · ${host}`}
        onClick={on_select}
      >
        {icon}
        <span className="terminal-tab-copy">{children}</span>
        <span className="terminal-tab-host">{host}</span>
      </button>
      <button
        className="terminal-tab-close"
        type="button"
        aria-label={close_label}
        title={close_hint}
        onClick={on_close}
      >
        <Icon name="close" size={14} />
      </button>
    </div>
  );
}

function navigateTabs(event: KeyboardEvent<HTMLDivElement>) {
  if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
  const buttons = Array.from(
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
  );
  const position = buttons.indexOf(event.target as HTMLButtonElement);
  if (position < 0) return;
  event.preventDefault();
  const index = event.key === "Home"
    ? 0
    : event.key === "End"
      ? buttons.length - 1
      : (position + (event.key === "ArrowRight" ? 1 : -1) + buttons.length) % buttons.length;
  buttons[index]?.focus();
  buttons[index]?.click();
}

export function TerminalTabs({
  extra_tabs = [],
  tab_order = [],
  on_select_extra,
  on_close_extra,
  tabs,
  shellStates,
  activeSessionKey,
  canCreate,
  onSelect,
  onClose,
  onCreate,
}: TerminalTabsProps) {
  const terminalNodes = tabs.map((tab) => {
    const key = sessionKey(tab);
    const title = formatTerminalTitle(tab, shellStates.get(key) ?? null);
    return {
      tab_key: key,
      node: (
        <WorkspaceTab
          key={key}
          active={key === activeSessionKey}
          title={title.text}
          host={targetLabel(tab.target)}
          close_label={`Close ${tab.name} tab`}
          close_hint="Close tab; keep session running"
          icon={<Icon name="terminal" class_name="tab-icon" />}
          on_select={() => onSelect(tab)}
          on_close={() => onClose(tab)}
        >
          <span className="terminal-tab-path"><bdi dir="ltr">{title.path}</bdi></span>
          {title.command ? (
            <>
              <span className="terminal-tab-separator" aria-hidden="true">—</span>
              <span className="terminal-tab-command">{title.command}</span>
            </>
          ) : null}
        </WorkspaceTab>
      ),
    };
  });
  const extraNodes = extra_tabs.map((tab) => ({
    tab_key: tab.tab_key,
    node: (
      <WorkspaceTab
        key={tab.tab_key}
        active={tab.tab_key === activeSessionKey}
        title={tab.title}
        host={tab.host}
        close_label={`Close ${tab.title} tab`}
        close_hint="Close view; keep task running"
        icon={<span className={`task-state-dot ${tab.status}`} />}
        on_select={() => on_select_extra?.(tab.tab_key)}
        on_close={() => on_close_extra?.(tab.tab_key)}
      >
        {tab.title}
      </WorkspaceTab>
    ),
  }));
  const nodes = [...terminalNodes, ...extraNodes].sort((a, b) => {
    const first = tab_order.indexOf(a.tab_key);
    const second = tab_order.indexOf(b.tab_key);
    return (first < 0 ? Infinity : first) - (second < 0 ? Infinity : second);
  });
  return (
    <nav className="terminal-tabs" aria-label="Workspace tabs">
      <div className="terminal-tab-list" role="tablist" onKeyDown={navigateTabs}>
        {nodes.map((item) => item.node)}
      </div>
      <button
        className="terminal-tab-new"
        type="button"
        aria-label="New tab in current folder"
        title="New tab in current folder"
        disabled={!canCreate}
        onClick={onCreate}
      >
        <Icon name="plus" />
      </button>
    </nav>
  );
}
