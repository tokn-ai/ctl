import { componentProtocolLines, type ComponentBuildView } from "../../features/about/componentVersions";
import type { ComponentVersionRow } from "../../lib/types";
import { Icon } from "../ui/Icon";

const protocol_labels: Record<string, string> = {
  ctmux: "Session", ctmux_control: "Local control", task: "Task", task_control: "Task control",
  ctld: "ctld IPC", ctld_lifecycle: "Lifecycle", ctl_identity: "Agent identity", ctl_maintenance: "Remote maintenance", ctld_helper: "Helper API",
};
const status_labels = { current: "Current", compatible: "Compatible", incompatible: "Incompatible", unknown: "Unverified" };

export function ComponentProtocols({ row, view }: { row: ComponentVersionRow; view: ComponentBuildView }) {
  const lines = componentProtocolLines(row, view);
  return lines.length ? <ul className="about-protocol-list" aria-label={`${view.label} protocols`}>{lines.map((protocol) => {
    const label = protocol_labels[protocol.name] ?? protocol.name;
    const status = status_labels[protocol.status];
    return <li key={protocol.name} className={`about-protocol-line about-protocol-${protocol.status}`} title={protocol.detail} aria-label={`${label} ${protocol.version}: ${status}`}>
      {protocol.status === "unknown" ? <span className="about-protocol-unknown" aria-hidden="true">?</span> : <Icon name={protocol.status === "incompatible" ? "close" : "check"} size={14} />}
      <span className="about-protocol-name">{label}</span><code>{protocol.version}</code><span className="about-protocol-state">{status}</span>
    </li>;
  })}</ul> : <span className="about-muted">Protocols not reported</span>;
}
