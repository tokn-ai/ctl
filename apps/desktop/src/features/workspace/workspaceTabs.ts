import type { SessionSummary } from "../../lib/types";
import { sessionKey } from "../targets/targets";
import { openTerminalTab } from "../tabs/tabState";
import { workspaceTabKey, type WorkspaceView } from "./workspaceModel";

/** Open and select a session without separating its terminal and workspace order. */
export function openWorkspaceSessionTab(
  view: WorkspaceView,
  session: SessionSummary,
  after_tab_key?: string,
): WorkspaceView {
  const identity = sessionKey(session);
  const already_open = view.tabs.some((tab) => sessionKey(tab) === identity);
  const tabs = openTerminalTab(view.tabs, session, after_tab_key);
  const keys = [...tabs.map(sessionKey), ...view.task_tabs.map(workspaceTabKey)];
  const tab_order = [
    ...view.tab_order.filter((key) => keys.includes(key)),
    ...keys.filter((key) => !view.tab_order.includes(key)),
  ];
  if (!already_open && after_tab_key !== undefined) {
    tab_order.splice(tab_order.indexOf(identity), 1);
    const anchor_index = tab_order.indexOf(after_tab_key);
    tab_order.splice(anchor_index < 0 ? tab_order.length : anchor_index + 1, 0, identity);
  }
  return { ...view, tabs, tab_order, active_tab_key: identity };
}
