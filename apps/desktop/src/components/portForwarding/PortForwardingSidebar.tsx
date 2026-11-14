import { useLocale } from "../../features/i18n/LocaleContext";
import { formatText as formatMessageText, translate, type DisplayText, type Locale } from "../../features/i18n/messages";
import type {
  SshConnectionTarget,
  WorkspacePortForward,
} from "../../lib/types";
import { targetLabel } from "../../features/targets/targets";
import type { PortForwardObservation } from "../../features/portForwarding/usePortForwarding";
import { portForwardProtocolLabel } from "../../features/portForwarding/protocol";
import { Icon } from "../ui/Icon";
import { InlineFeedback } from "../feedback/InlineFeedback";
import "./portForwarding.css";

interface Props {
  targets: readonly SshConnectionTarget[];
  forwards: readonly WorkspacePortForward[];
  statuses: ReadonlyMap<string, PortForwardObservation>;
  busy: ReadonlySet<string>;
  hostErrors: ReadonlyMap<string, DisplayText>;
  refreshing: boolean;
  lastRefreshedAt: number | null;
  onRefresh(): void;
  onSetEnabled(
    target: SshConnectionTarget,
    forward: WorkspacePortForward,
    enabled: boolean,
  ): void;
  onManage(target: SshConnectionTarget): void;
}

export function PortForwardingSidebar({
  targets,
  forwards,
  statuses,
  busy,
  hostErrors,
  refreshing,
  lastRefreshedAt,
  onRefresh,
  onSetEnabled,
  onManage,
}: Props) {
  const { t, locale, formatText } = useLocale();
  const groups = targets
    .map((target) => ({
      target,
      forwards: forwards
        .filter((forward) => forward.host_id === target.host_id)
        .sort((left, right) =>
          left.local_port - right.local_port || left.name.localeCompare(right.name, locale),
        ),
    }))
    .filter((group) => group.forwards.length > 0);

  return (
    <aside className="port-sidebar" aria-label={t("ports.title")}>
      <header className="sidebar-header">
        <strong>{t("ports.title")}</strong>
        <span className="port-sidebar-count">{forwards.length.toLocaleString(locale)}</span>
        <button
          className="icon-button"
          type="button"
          onClick={onRefresh}
          disabled={refreshing}
          aria-label={t("ports.refresh_status")}
          title={t("ports.refresh_status")}
        >
          <Icon name="refresh" class_name={refreshing ? "port-refreshing" : undefined} />
        </button>
      </header>

      <div className="port-sidebar-list">
        {groups.length === 0 ? (
          <div className="sidebar-state">
            <Icon name="ports" size={28} class_name="empty-glyph" />
            <p>{t("ports.empty_title")}</p>
            <small>{t("ports.empty_hint")}</small>
          </div>
        ) : null}
        {groups.map(({ target, forwards: hostForwards }) => {
          const error = hostErrors.get(target.host_id!);
          return (
            <section
              className="port-sidebar-group"
              key={target.host_id}
              aria-label={t("ports.host_forwards", { host: targetLabel(target, locale) })}
            >
              <header>
                <h3>{targetLabel(target, locale)} <span>{hostForwards.length.toLocaleString(locale)}</span></h3>
                <button type="button" onClick={() => onManage(target)}>{t("ports.manage")}</button>
              </header>
              {error ? (
                <InlineFeedback severity="warning" action={{ label: t("ports.retry_host"), on_click: onRefresh, disabled: refreshing }}>
                  {formatText(error)}
                </InlineFeedback>
              ) : null}
              {hostForwards.map((forward) => {
                const changing = busy.has(forward.forward_id);
                const status = forwardingStatus(forward, statuses.get(forward.forward_id), locale);
                return (
                  <div className="port-sidebar-row" key={forward.forward_id}>
                    <button
                      className="port-sidebar-details"
                      type="button"
                      onClick={() => onManage(target)}
                      title={t("ports.manage_forward", { name: forward.name })}
                    >
                      <span className={`port-sidebar-indicator ${status.tone}`} aria-hidden="true" />
                      <span>
                        <strong>{forward.name}</strong>
                        <code>{portForwardProtocolLabel(forward.protocol)} · {forward.bind_address}:{forward.local_port}</code>
                        <small>→ {forward.remote_host}:{forward.remote_port}</small>
                        <small className={`port-sidebar-status ${status.tone}`}>
                          {changing ? t("ports.sidebar_changing") : status.label}
                        </small>
                      </span>
                    </button>
                    <button
                      className="port-sidebar-toggle"
                      type="button"
                      disabled={changing}
                      onClick={() => onSetEnabled(target, forward, !forward.enabled)}
                    >
                      {forward.enabled ? t("ports.stop") : t("ports.start")}
                    </button>
                  </div>
                );
              })}
            </section>
          );
        })}
      </div>

      <footer className="port-sidebar-footer">
        <small>
          {lastRefreshedAt === null
            ? t("ports.unchecked")
            : t("ports.update_time", { time: new Date(lastRefreshedAt).toLocaleTimeString(locale, {
                hour: "2-digit",
                minute: "2-digit",
              }) })}
        </small>
      </footer>
    </aside>
  );
}

function forwardingStatus(
  forward: WorkspacePortForward,
  status: PortForwardObservation | undefined,
  locale: Locale,
): { label: string; tone: "active" | "waiting" | "error" | "stopped" } {
  if (!forward.enabled) return { label: translate(locale, "ports.stopped"), tone: "stopped" };
  if (!status) return { label: translate(locale, "ports.checking"), tone: "waiting" };
  switch (status.state) {
    case "active":
      return { label: translate(locale, "ports.active"), tone: "active" };
    case "error":
      return { label: status.message ? formatMessageText(locale, status.message) : translate(locale, "ports.error"), tone: "error" };
    case "waiting_for_authentication":
      return { label: status.message ? formatMessageText(locale, status.message) : translate(locale, "ports.waiting_ssh"), tone: "waiting" };
  }
}
