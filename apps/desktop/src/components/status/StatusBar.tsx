import type { AttachmentViewState } from "../../lib/types";
import {
  createStatusGroups,
  type StatusItem,
} from "./statusItems";

interface StatusBarProps {
  state: AttachmentViewState;
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

export function StatusBar({ state }: StatusBarProps) {
  const groups = createStatusGroups(state);

  return (
    <footer className="status-bar" aria-label="Terminal status">
      <div className="status-group status-context">
        {groups.context.map((status) => (
          <StatusEntry key={status.key} status={status} />
        ))}
      </div>
      <div className="status-group status-indicators">
        {groups.indicators.map((status) => (
          <StatusEntry key={status.key} status={status} />
        ))}
      </div>
    </footer>
  );
}
