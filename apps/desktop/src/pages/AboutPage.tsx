import { translate, type Locale } from "../features/i18n/messages";
import { useLocale } from "../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ComponentBundlePanel } from "../components/about/ComponentBundlePanel";
import { ComponentVersionTable, versionLabel } from "../components/about/ComponentVersionTable";
import { QuickInput } from "../components/commands/QuickInput";
import { Icon } from "../components/ui/Icon";
import { InlineFeedback } from "../components/feedback/InlineFeedback";
import { UpdateComponentsDialog } from "../components/components/UpdateComponentsDialog";
import { PublishedBundleStatus } from "../components/components/PublishedBundleStatus";
import { SshHostFlow } from "../components/sessions/SshHostFlow";
import { useComponentVersions } from "../features/about/useComponentVersions";
import type { ComponentActionPreflight, ComponentActionResult, ComponentUpdateContext, ConnectionTarget, SshConnectionTarget } from "../lib/types";
import "../components/about/about.css";

interface Props {
  visible: boolean;
  on_close(): void;
  on_restarted(preflight: ComponentActionPreflight): void;
  execute_action?(preflight: ComponentActionPreflight): Promise<ComponentActionResult>;
  on_dialog_change(open: boolean): void;
  remote_targets?: readonly ConnectionTarget[];
}

function actionDescription(preflight: ComponentActionPreflight, locale: Locale): string {
  const { kind, active_transports, description } = preflight.impact;
  const impact_id = kind === "remote_reconnect" && !preflight.available
    ? "about.impact_remote_reconnect_unverified"
    : kind ? `about.impact_${kind}` : null;
  return translate(locale, "about.action_description", {
    impact: impact_id ? translate(locale, impact_id, { count: active_transports ?? 0 }) : description,
    running: versionLabel(preflight.running, locale),
    replacement: versionLabel(preflight.available, locale),
  });
}

/** Remains mounted when hidden so a requested restart is observed to completion. */
export function AboutPage({ visible, on_close, on_restarted, on_dialog_change, execute_action, remote_targets = [] }: Props) {
  const { t, locale, intl, formatText } = useLocale();
  const model = useComponentVersions(visible, on_restarted, execute_action);
  const [component_flow, setComponentFlow] = useState<{ target: ConnectionTarget; mode: "inspect" | "update" } | null>(null);
  const [update_context, setUpdateContext] = useState<ComponentUpdateContext | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { if (visible) heading.current?.focus(); }, [visible]);
  const has_dialog = model.preflight !== null || component_flow !== null || update_context !== null;
  useEffect(() => { on_dialog_change(has_dialog); }, [has_dialog, on_dialog_change]);
  const app = model.snapshot?.components.find((row) => row.component === "ctmux");
  const local = model.snapshot?.components.filter((row) => row.location === "local" && row.component !== "ctmux") ?? [];
  const remote = model.snapshot?.components.filter((row) => row.location === "remote") ?? [];
  const manageable_targets = remote_targets.filter((target): target is SshConnectionTarget =>
    target.kind === "ssh" && target.remote_platform !== "windows");
  const table_props = { busy_id: model.busy_id, restarting: model.restarting, action_error: model.action_error, on_restart: (id: string) => void model.requestRestart(id) };

  return <>
    <section className="about-page" aria-label={t("about.title")} hidden={!visible}>
      <header className="about-page-header">
        <div><p className="about-eyebrow">CTMUX</p><h1 ref={heading} tabIndex={-1}>{t("about.title")}</h1><p className="about-muted">{t("about.intro")}</p></div>
        <button type="button" onClick={on_close} className="about-back"><Icon name="close" />{t("about.back")}</button>
      </header>
      <div className="about-content">
        <div className="about-app-card"><Icon name="terminal" size={36} /><div><strong>ctmux</strong><p>{app ? versionLabel(app.running, locale) : model.loading ? t("about.checking_app") : t("about.unknown_version")}</p><small>{t("about.desktop")}</small></div></div>
        <section aria-label={t("about.version_checks")}>
          <div className="about-refresh-row"><button type="button" onClick={() => setUpdateContext({})}>{t("about.action_update")}</button><p>{model.checked_at === null ? t("about.unchecked") : t("about.last_checked", { time: intl.formatTime(model.checked_at, { hour: "2-digit", minute: "2-digit", second: "2-digit" }) })}</p><button type="button" onClick={() => void model.refresh()} disabled={model.loading || model.busy_id !== null}><Icon name="refresh" />{model.loading ? t("about.checking") : t("about.refresh_versions")}</button></div>
          {model.error ? <InlineFeedback>{t(model.snapshot ? "about.refresh_failed_cached" : "about.refresh_failed", { detail: model.error })}</InlineFeedback> : null}
          {model.action_error && !model.snapshot?.components.some((row) => row.component_id === model.action_error!.component_id) ? <InlineFeedback>{formatText(model.action_error.message)}</InlineFeedback> : null}
          {model.loading && !model.snapshot ? <p role="status">{t("about.checking_local_remote")}</p> : null}
        </section>
        <PublishedBundleStatus visible={visible} on_update={(channel) => setUpdateContext({ source: { kind: "published", channel } })} />
        <ComponentBundlePanel visible={visible} on_selected={() => void model.refresh()} />
        <section className="about-section" aria-labelledby="about-local"><h2 id="about-local">{t("about.local_components")}</h2><p className="about-muted">{t("about.local_help")}</p>{local.length ? <ComponentVersionTable rows={local} {...table_props} /> : !model.loading ? <p className="about-muted">{t("about.no_local_components")}</p> : null}</section>
        <section className="about-section" aria-labelledby="about-remote"><h2 id="about-remote">{t("about.remote_hosts")}</h2><p className="about-muted">{t("about.remote_help")}</p>{remote.length ? <ComponentVersionTable rows={remote} {...table_props} on_manage_host={(host_id, mode) => {
          const target = manageable_targets.find((target) => target.host_id === host_id);
          if (target) { if (mode === "update") setUpdateContext({ targets: [target] }); else setComponentFlow({ target, mode }); }
        }} manageable_host_ids={manageable_targets.flatMap((target) => target.host_id ? [target.host_id] : [])} /> : !model.loading ? <p className="about-empty">{t("about.no_remote_hosts")}</p> : null}</section>
        <div className="about-protocol-legend" aria-label={t("about.protocol_legend")}>
          <span className="about-protocol-current"><Icon name="check" size={14} />{t("about.current_protocol")}</span>
          <span className="about-protocol-compatible"><Icon name="check" size={14} />{t("about.compatible_protocol")}</span>
          <span className="about-protocol-incompatible"><Icon name="close" size={14} />{t("about.incompatible_protocol")}</span>
          <span className="about-muted">{t("about.unverified_protocol")}</span>
        </div>
        <p className="about-footnote">{t("about.comparison_footnote")}</p>
      </div>
    </section>
    {model.preflight ? createPortal(<QuickInput title={t(model.preflight.action === "reconnect" ? "about.reconnect_component" : "about.restart_component", { name: model.preflight.label })} description={actionDescription(model.preflight, locale)} mode={{ kind: "confirm", confirm_label: t(model.preflight.action === "reconnect" ? "about.reconnect_component" : "about.restart_component", { name: model.preflight.component === "ctl_agent" ? "ctl-agent" : model.preflight.component }), destructive: model.preflight.action === "restart" }} onCancel={model.cancelRestart} onSubmit={model.confirmRestart} />, document.body) : null}
    {update_context ? createPortal(<UpdateComponentsDialog targets={remote_targets} context={update_context} on_updated={() => {
      void model.refresh();
    }} on_close={() => setUpdateContext(null)} />, document.body) : null}
    {component_flow ? <SshHostFlow suggestions={[]} warning={null} target={component_flow.target} component_mode={component_flow.mode} autoConnect={component_flow.mode === "inspect"} updateRequired={component_flow.mode === "update"} on_components_complete={() => {
      void model.refresh(component_flow.target.kind === "ssh" ? component_flow.target.host_id : undefined);
    }} onClose={() => setComponentFlow(null)} /> : null}
  </>;
}
