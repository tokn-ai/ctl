import { translate, type Locale } from "../features/i18n/messages";
import { useLocale } from "../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ComponentBundlePanel } from "../components/about/ComponentBundlePanel";
import { versionLabel } from "../components/about/ComponentVersionTable";
import { QuickInput } from "../components/commands/QuickInput";
import { Icon } from "../components/ui/Icon";
import { InlineFeedback } from "../components/feedback/InlineFeedback";
import { UpdateComponentsDialog } from "../components/components/UpdateComponentsDialog";
import { RuntimeMachines } from "../components/about/RuntimeMachines";
import { SshHostFlow } from "../components/sessions/SshHostFlow";
import { useComponentVersions } from "../features/about/useComponentVersions";
import { LOCAL_TARGET, supportsComponentUpdate, targetKey } from "../features/targets/targets";
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
  const [inspect_target, setInspectTarget] = useState<SshConnectionTarget | null>(null);
  const [builds_revision, setBuildsRevision] = useState(0);
  const [builds_open, setBuildsOpen] = useState(false);
  const [revisions, setRevisions] = useState<ReadonlyMap<string, number>>(() => new Map());
  const [update_context, setUpdateContext] = useState<ComponentUpdateContext | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { if (visible) heading.current?.focus(); }, [visible]);
  const preflight = model.preflight;
  const action_title = preflight?.action === "reconnect" ? "about.reconnect_component" : "about.restart_component";
  const has_dialog = preflight !== null || inspect_target !== null || update_context !== null;
  useEffect(() => { on_dialog_change(has_dialog); }, [has_dialog, on_dialog_change]);
  const app = model.snapshot?.components.find((row) => row.component === "ctmux");
  const rows = model.snapshot?.components ?? [];

  function refreshInstallations(targets: readonly ConnectionTarget[]) {
    setRevisions((previous) => {
      const next = new Map(previous);
      for (const target of targets) {
        const key = targetKey(target);
        next.set(key, (next.get(key) ?? 0) + 1);
      }
      return next;
    });
    void model.refresh();
  }

  return <>
    <section className="about-page" aria-label={t("about.title")} hidden={!visible}>
      <header className="about-page-header">
        <div>
          <h1 ref={heading} tabIndex={-1}>{t("about.title")}</h1>
          <p className="about-muted">{app ? versionLabel(app.running, locale) : model.loading ? t("about.checking_app") : t("about.unknown_version")}</p>
        </div>
        <button type="button" onClick={on_close} className="about-back"><Icon name="close" />{t("about.back")}</button>
      </header>
      <div className="about-content">
        <RuntimeMachines
          visible={visible}
          rows={rows}
          targets={remote_targets}
          refresh_revision={revisions}
          busy_id={model.busy_id}
          restarting={model.restarting}
          action_error={model.action_error}
          on_restart={(id) => void model.requestRestart(id)}
          on_inspect={setInspectTarget}
          on_install={(target) => setUpdateContext({ targets: [target] })}
          on_synced={(target) => {
            void model.refresh(target.kind === "ssh" ? target.host_id : undefined);
            if (target.kind === "local") setBuildsRevision((value) => value + 1);
          }}
        />
        <section aria-label={t("about.version_checks")}>
          <div className="about-refresh-row">
            <p>{model.checked_at === null ? t("about.unchecked") : t("about.last_checked", { time: intl.formatTime(model.checked_at, { hour: "2-digit", minute: "2-digit", second: "2-digit" }) })}</p>
            <button type="button" onClick={() => void model.refresh()} disabled={model.loading || model.busy_id !== null}>
              <Icon name="refresh" />{t(model.loading ? "about.checking" : "about.refresh_versions")}
            </button>
          </div>
          {model.error ? <InlineFeedback>{t(model.snapshot ? "about.refresh_failed_cached" : "about.refresh_failed", { detail: model.error })}</InlineFeedback> : null}
          {model.action_error && !rows.some((row) => row.component_id === model.action_error?.component_id) ? <InlineFeedback>{formatText(model.action_error.message)}</InlineFeedback> : null}
          {model.loading && !model.snapshot ? <p role="status">{t("about.checking_local_remote")}</p> : null}
        </section>
        {supportsComponentUpdate(LOCAL_TARGET) ? <details className="about-stored-builds" onToggle={(event) => setBuildsOpen(event.currentTarget.open)}>
          <summary>{t("about.runtime_stored_builds")}</summary>
          <ComponentBundlePanel visible={visible && builds_open} refresh_revision={builds_revision} on_selected={() => refreshInstallations([LOCAL_TARGET])} />
        </details> : null}
      </div>
    </section>
    {preflight && createPortal(<QuickInput
      title={t(action_title, { name: preflight.label })}
      description={actionDescription(preflight, locale)}
      mode={{
        kind: "confirm",
        confirm_label: t(action_title, { name: preflight.component === "ctl_agent" ? "ctl-agent" : preflight.component }),
        destructive: preflight.action === "restart",
      }}
      onCancel={model.cancelRestart}
      onSubmit={model.confirmRestart}
    />, document.body)}
    {update_context && createPortal(<UpdateComponentsDialog
      targets={remote_targets}
      context={update_context}
      install_from
      on_updated={(results, targets) => {
        const completed = results.filter((result) => result.state === "complete").map((result) => targets[result.host_index]);
        if (completed.some((target) => target.kind === "local")) setBuildsRevision((value) => value + 1);
        if (completed.length) refreshInstallations(completed);
      }}
      on_close={() => setUpdateContext(null)}
    />, document.body)}
    {inspect_target && <SshHostFlow
      suggestions={[]}
      warning={null}
      target={inspect_target}
      component_mode="inspect"
      autoConnect
      on_components_complete={() => void model.refresh(inspect_target.host_id)}
      onClose={() => setInspectTarget(null)}
    />}
  </>;
}
