import type { AttachmentViewState } from "../../lib/types";
import type { ReactNode } from "react";
import { targetLabel } from "../../features/targets/targets";
import { ControlPopover } from "../ui/ControlPopover";
import { TerminalStatistics } from "./TerminalStatistics";
import { createStatusGroups } from "./statusItems";

interface StatusBarProps {
  state: AttachmentViewState;
  children?: ReactNode;
  show_terminal?: boolean;
  context_label?: string;
  inert?: boolean;
}

export function StatusBar({ state, children, show_terminal = true, context_label = "Tasks", inert }: StatusBarProps) {
  const groups = show_terminal ? createStatusGroups(state) : { context: [], indicators: [] };
  // Routine metadata is available on demand. Connection and history problems stay visible.
  const alerts = groups.indicators.filter((item) => item.key === "connection" || item.key === "history");
  const details = [...groups.context, ...groups.indicators];
  const host = state.session ? targetLabel(state.session.target) : "No session";

  return (
    <footer className="status-bar" aria-label="Workspace status" inert={inert}>
      <div className="status-session">
        {show_terminal ? <ControlPopover label="Session details" trigger_content={<>
          <span className={`connection-dot ${state.phase}`} aria-hidden="true" />{host}
        </>}>
          {details.length ? details.map((item) => <span key={item.key} title={item.title}>{item.title}</span>) : <span>No active session</span>}
        </ControlPopover> : <span className="status-item">{context_label}</span>}
      </div>
      <div className="status-alerts" role="status">
        {alerts.map((item) => <span key={item.key} className={`status-item status-tone-${item.tone}`} title={item.title}>{item.label.toLowerCase()}</span>)}
      </div>
      <TerminalStatistics session={show_terminal ? state.session : null} inert={inert} />
      {children}
    </footer>
  );
}
