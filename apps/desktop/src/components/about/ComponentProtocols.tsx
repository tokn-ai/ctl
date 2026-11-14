import { useLocale } from "../../features/i18n/LocaleContext";
import { componentProtocolLines, type ComponentBuildView } from "../../features/about/componentVersions";
import type { ComponentVersionRow } from "../../lib/types";
import { Icon } from "../ui/Icon";

const protocol_labels: Record<string, string> = {
  ctmux: "about.protocol_session", ctmux_control: "about.protocol_local_control", task: "about.protocol_task", task_control: "about.protocol_task_control",
  ctld: "about.protocol_ctld_ipc", ctld_lifecycle: "about.protocol_lifecycle", ctl_identity: "about.protocol_agent_identity", ctl_maintenance: "about.protocol_maintenance", ctld_helper: "about.protocol_helper_api",
};
const status_labels = { current: "about.current", compatible: "about.compatible", incompatible: "about.incompatible", unknown: "about.unverified" };

export function ComponentProtocols({ row, view }: { row: ComponentVersionRow; view: ComponentBuildView }) {
  const { t, locale } = useLocale();
  const lines = componentProtocolLines(row, view, locale);
  return lines.length ? <ul className="about-protocol-list" aria-label={t("about.protocols_for_build", { build: view.label })}>{lines.map((protocol) => {
    const label = protocol_labels[protocol.name] ? t(protocol_labels[protocol.name]) : protocol.name;
    const status = t(status_labels[protocol.status]);
    return <li key={protocol.name} className={`about-protocol-line about-protocol-${protocol.status}`} title={protocol.detail} aria-label={t("about.protocol_info", { label, version: protocol.version, status })}>
      <span className="about-protocol-marker" aria-hidden="true">{protocol.status === "unknown" ? "?" : <Icon name={protocol.status === "incompatible" ? "close" : "check"} size={14} />}</span>
      <span className="about-protocol-name">{label}</span><code>{protocol.version}</code><span className="about-protocol-state">{status}</span>
    </li>;
  })}</ul> : <span className="about-muted">{t("about.protocols_not_reported")}</span>;
}
