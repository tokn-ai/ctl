import type { AttachmentViewState } from "../../lib/types";
import type { ReactNode } from "react";
import {
  createStatusGroups,
  type StatusItem,
} from "./statusItems";

interface StatusBarProps {
  state: AttachmentViewState;
  children?: ReactNode;
  show_terminal?: boolean;
  inert?: boolean;
}

function StatusEntry({ status }: { status: StatusItem }) {
  const classNames = [
    "status-item",
    `status-priority-${status.priority}`,
    `status-tone-${status.tone}`,
  ];
  if (status.flexible) {
    classNames.push("status-flexible");
  }

  return (
    <span className={classNames.join(" ")} title={status.title}>
      {status.label}
    </span>
  );
}

export function StatusBar({ state, children, show_terminal = true, inert }: StatusBarProps) {
  const groups = show_terminal ? createStatusGroups(state) : { context: [], indicators: [] };

  return (
    <footer className="status-bar" aria-label="Workspace status" inert={inert}>
      <div className="status-group status-context">
        {groups.context.map((status) => (
          <StatusEntry key={status.key} status={status} />
        ))}
        {!show_terminal ? <span className="status-item">Tasks</span> : null}
      </div>
      <div className="status-group status-indicators">
        {groups.indicators.map((status) => (
          <StatusEntry key={status.key} status={status} />
        ))}
      </div>
      {children}
    </footer>
  );
}
