import type { SessionSummary, ShellStateSummary } from "../../lib/types";
import { formatTerminalTitle } from "../../features/tabs/terminalTitle";
import { sessionKey, targetLabel } from "../../features/targets/targets";

export interface ExtraTab { tab_key: string; title: string; host: string; status: string }
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

export function TerminalTabs({
  extra_tabs = [], tab_order = [], on_select_extra, on_close_extra,
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
    return { tab_key: key, node: <div className={`terminal-tab ${key === activeSessionKey ? "active" : ""}`} key={key}>
      <button className="terminal-tab-select" type="button" role="tab" aria-selected={key === activeSessionKey} aria-label={`${title.text} on ${targetLabel(tab.target)}`} title={`${title.text} · ${targetLabel(tab.target)}`} onClick={() => onSelect(tab)}>
        <span className="terminal-tab-dot" aria-hidden="true" /><span className="terminal-tab-host">{targetLabel(tab.target)}</span><span className="terminal-tab-copy"><span className="terminal-tab-path"><bdi dir="ltr">{title.path}</bdi></span>{title.command ? <><span className="terminal-tab-separator" aria-hidden="true">—</span><span className="terminal-tab-command">{title.command}</span></> : null}</span>
      </button><button className="terminal-tab-close" type="button" aria-label={`Close ${tab.name} tab`} title="Close tab; keep session running" onClick={() => onClose(tab)}>×</button>
    </div> };
  });
  const extraNodes = extra_tabs.map((tab) => ({ tab_key: tab.tab_key, node: <div className={`terminal-tab ${tab.tab_key === activeSessionKey ? "active" : ""}`} key={tab.tab_key}>
    <button className="terminal-tab-select" type="button" role="tab" aria-selected={tab.tab_key === activeSessionKey} aria-label={`${tab.title} on ${tab.host}`} onClick={() => on_select_extra?.(tab.tab_key)}><span className={`task-state-dot ${tab.status}`} /><span className="terminal-tab-host">{tab.host}</span><span className="terminal-tab-copy">{tab.title}</span></button>
    <button className="terminal-tab-close" type="button" aria-label={`Close ${tab.title} tab`} title="Close view; keep task running" onClick={() => on_close_extra?.(tab.tab_key)}>×</button>
  </div> }));
  const nodes = [...terminalNodes, ...extraNodes].sort((a, b) => {
    const first = tab_order.indexOf(a.tab_key), second = tab_order.indexOf(b.tab_key);
    return (first < 0 ? Infinity : first) - (second < 0 ? Infinity : second);
  });
  return <nav className="terminal-tabs" aria-label="Workspace tabs"><div className="terminal-tab-list" role="tablist" onKeyDown={(event) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
    const position = buttons.indexOf(event.target as HTMLButtonElement);
    if (position < 0) return;
    event.preventDefault();
    const index = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : (position + (event.key === "ArrowRight" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[index]?.focus(); buttons[index]?.click();
  }}>{nodes.map((item) => item.node)}</div><button className="terminal-tab-new" type="button" aria-label="New tab in current folder" title="New tab in current folder" disabled={!canCreate} onClick={onCreate}>+</button></nav>;
}
