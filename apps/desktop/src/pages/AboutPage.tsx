import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ComponentBundlePanel } from "../components/about/ComponentBundlePanel";
import { ComponentVersionTable, versionLabel } from "../components/about/ComponentVersionTable";
import { QuickInput } from "../components/commands/QuickInput";
import { Icon } from "../components/ui/Icon";
import { SshHostFlow } from "../components/sessions/SshHostFlow";
import { useComponentVersions } from "../features/about/useComponentVersions";
import type { ComponentActionPreflight, ComponentActionResult, ConnectionTarget } from "../lib/types";
import "../components/about/about.css";

interface Props {
  visible: boolean;
  on_close(): void;
  on_restarted(preflight: ComponentActionPreflight): void;
  execute_action?(preflight: ComponentActionPreflight): Promise<ComponentActionResult>;
  on_dialog_change(open: boolean): void;
  remote_targets?: readonly ConnectionTarget[];
}

function actionDescription(preflight: ComponentActionPreflight): string {
  return `${preflight.impact.description} Running: ${versionLabel(preflight.running)}. Replacement: ${versionLabel(preflight.available)}. Confirm within 20 seconds; otherwise check the component again.`;
}

/** Remains mounted when hidden so a requested restart is observed to completion. */
export function AboutPage({ visible, on_close, on_restarted, on_dialog_change, execute_action, remote_targets = [] }: Props) {
  const model = useComponentVersions(visible, on_restarted, execute_action);
  const [component_flow, setComponentFlow] = useState<{ target: ConnectionTarget; mode: "inspect" | "update" } | null>(null);
  const [component_notice, setComponentNotice] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { if (visible) heading.current?.focus(); }, [visible]);
  const has_dialog = model.preflight !== null || component_flow !== null;
  useEffect(() => { on_dialog_change(has_dialog); }, [has_dialog, on_dialog_change]);
  const app = model.snapshot?.components.find((row) => row.component === "ctmux");
  const local = model.snapshot?.components.filter((row) => row.location === "local" && row.component !== "ctmux") ?? [];
  const remote = model.snapshot?.components.filter((row) => row.location === "remote") ?? [];
  const table_props = { busy_id: model.busy_id, restarting: model.restarting, action_error: model.action_error, on_restart: (id: string) => void model.requestRestart(id) };

  return <>
    <section className="about-page" aria-label="About ctmux" hidden={!visible}>
      <header className="about-page-header">
        <div><p className="about-eyebrow">CTMUX</p><h1 ref={heading} tabIndex={-1}>About ctmux</h1><p className="about-muted">Installed components, running processes, and updates.</p></div>
        <button type="button" onClick={on_close} className="about-back"><Icon name="close" />Back to workspace</button>
      </header>
      <div className="about-content">
        <div className="about-app-card"><Icon name="terminal" size={36} /><div><strong>ctmux</strong><p>{app ? versionLabel(app.running) : model.loading ? "Checking app version…" : "Version unavailable"}</p><small>Desktop application</small></div></div>
        <div className="about-refresh-row"><p>{model.checked_at === null ? "Component versions have not been checked." : `Last checked ${new Date(model.checked_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}`}</p><button type="button" onClick={() => void model.refresh()} disabled={model.loading || model.busy_id !== null}><Icon name="refresh" />{model.loading ? "Checking…" : "Refresh versions"}</button></div>
        {model.error ? <p className="about-error" role="alert">Could not refresh versions: {model.error}{model.snapshot ? " Showing the last successful check." : ""}</p> : null}
        {model.action_error && !model.snapshot?.components.some((row) => row.component_id === model.action_error!.component_id) ? <p className="about-error" role="alert">{model.action_error.message}</p> : null}
        {model.notice ? <p className="about-notice" role="status">{model.notice}</p> : null}
        {component_notice ? <p className="about-notice" role="status">{component_notice}</p> : null}
        {model.loading && !model.snapshot ? <p role="status">Checking local components and saved hosts…</p> : null}
        <ComponentBundlePanel visible={visible} on_selected={() => void model.refresh()} />
        <section className="about-section" aria-labelledby="about-local"><h2 id="about-local">Local components</h2><p className="about-muted">On disk shows the selected local build. Expand this computer to compare it with running services, then restart separately.</p>{local.length ? <ComponentVersionTable rows={local} {...table_props} /> : !model.loading ? <p className="about-muted">Local component versions are unavailable.</p> : null}</section>
        <section className="about-section" aria-labelledby="about-remote"><h2 id="about-remote">Remote hosts</h2><p className="about-muted">Refresh uses existing SSH connections, including hosts whose terminal daemon cannot reply. Check host can authenticate through its preferred route. Updating installs verified components and keeps sessions; Restart applies the installed terminal daemon after confirmation.</p>{remote.length ? <ComponentVersionTable rows={remote} {...table_props} on_manage_host={(host_id, mode) => {
          const target = remote_targets.find((target) => target.kind === "ssh" && target.host_id === host_id);
          if (target) { setComponentNotice(null); setComponentFlow({ target, mode }); }
        }} manageable_host_ids={remote_targets.flatMap((target) => target.kind === "ssh" && target.host_id ? [target.host_id] : [])} /> : !model.loading ? <p className="about-empty">No saved or connected remote hosts.</p> : null}</section>
        <div className="about-protocol-legend" aria-label="Protocol status legend">
          <span className="about-protocol-current"><Icon name="check" size={14} />Current protocol</span>
          <span className="about-protocol-compatible"><Icon name="check" size={14} />Compatible, different version</span>
          <span className="about-protocol-incompatible"><Icon name="close" size={14} />Incompatible protocol</span>
          <span className="about-muted">? Unreported or unverified</span>
        </div>
        <p className="about-footnote">“Different build” means the source differs; it does not establish which build is newer. Unknown versions cannot be compared.</p>
      </div>
    </section>
    {model.preflight ? createPortal(<QuickInput title={`${model.preflight.action === "reconnect" ? "Reconnect" : "Restart"} ${model.preflight.label}`} description={actionDescription(model.preflight)} mode={{ kind: "confirm", confirm_label: `${model.preflight.action === "reconnect" ? "Reconnect" : "Restart"} ${model.preflight.component === "ctl_agent" ? "ctl-agent" : model.preflight.component}`, destructive: model.preflight.action === "restart" }} onCancel={model.cancelRestart} onSubmit={model.confirmRestart} />, document.body) : null}
    {component_flow ? <SshHostFlow suggestions={[]} warning={null} target={component_flow.target} component_mode={component_flow.mode} autoConnect={component_flow.mode === "inspect"} updateRequired={component_flow.mode === "update"} on_components_complete={(updated) => {
      setComponentNotice(updated ? "Verified components installed. Running sessions were preserved. Restart separately where required." : "SSH account verified. Refreshing component status.");
      void model.refresh(component_flow.target.kind === "ssh" ? component_flow.target.host_id : undefined);
    }} onClose={() => setComponentFlow(null)} /> : null}
  </>;
}
