import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { ComponentVersionTable, versionLabel } from "../components/about/ComponentVersionTable";
import { QuickInput } from "../components/commands/QuickInput";
import { Icon } from "../components/ui/Icon";
import { useComponentVersions } from "../features/about/useComponentVersions";
import type { CtldRestartPreflight } from "../lib/types";
import "../components/about/about.css";

interface Props {
  visible: boolean;
  on_close(): void;
  on_restarted(): void;
  on_dialog_change(open: boolean): void;
}

function restartDescription(preflight: CtldRestartPreflight): string {
  const vpn_count = preflight.impact.vpn_connections;
  const vpn = vpn_count === null ? "Stops managed VPN connections" : vpn_count === 0 ? "Interrupts the connection broker" : `Stops ${vpn_count} managed VPN connection${vpn_count === 1 ? "" : "s"}`;
  return `${vpn} and may interrupt SSH connections and port forwards. VPNs can be reconnected afterward. Running: ${versionLabel(preflight.running)}. Replacement: ${versionLabel(preflight.available)}.`;
}

/** Remains mounted when hidden so a requested restart is observed to completion. */
export function AboutPage({ visible, on_close, on_restarted, on_dialog_change }: Props) {
  const model = useComponentVersions(visible, on_restarted);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { if (visible) heading.current?.focus(); }, [visible]);
  const has_dialog = model.preflight !== null;
  useEffect(() => { on_dialog_change(has_dialog); }, [has_dialog, on_dialog_change]);
  const app = model.snapshot?.components.find((row) => row.component === "rmux");
  const local = model.snapshot?.components.filter((row) => row.location === "local" && row.component !== "rmux") ?? [];
  const remote = model.snapshot?.components.filter((row) => row.location === "remote") ?? [];
  const table_props = { busy_id: model.busy_id, restarting: model.restarting, action_error: model.action_error, on_restart: (id: string) => void model.requestRestart(id) };

  return <>
    <section className="about-page" aria-label="About rmux" hidden={!visible}>
      <header className="about-page-header">
        <div><p className="about-eyebrow">RMUX</p><h1 ref={heading} tabIndex={-1}>About rmux</h1><p className="about-muted">Versions of this app and the components it uses.</p></div>
        <button type="button" onClick={on_close} className="about-back"><Icon name="close" />Back to workspace</button>
      </header>
      <div className="about-content">
        <div className="about-app-card"><Icon name="terminal" size={36} /><div><strong>rmux</strong><p>{app ? versionLabel(app.running) : model.loading ? "Checking app version…" : "Version unavailable"}</p><small>Desktop application</small></div></div>
        <div className="about-refresh-row"><p>{model.checked_at === null ? "Component versions have not been checked." : `Last checked ${new Date(model.checked_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}`}</p><button type="button" onClick={() => void model.refresh()} disabled={model.loading || model.busy_id !== null}><Icon name="refresh" />{model.loading ? "Checking…" : "Refresh versions"}</button></div>
        {model.error ? <p className="about-error" role="alert">Could not refresh versions: {model.error}{model.snapshot ? " Showing the last successful check." : ""}</p> : null}
        {model.action_error && !model.snapshot?.components.some((row) => row.component_id === model.action_error!.component_id) ? <p className="about-error" role="alert">{model.action_error.message}</p> : null}
        {model.notice ? <p className="about-notice" role="status">{model.notice}</p> : null}
        {model.loading && !model.snapshot ? <p role="status">Checking local components and connected hosts…</p> : null}
        <section className="about-section" aria-labelledby="about-local"><h2 id="about-local">This computer</h2><p className="about-muted">Versions compared with this app’s build. Available shows the selected local helper.</p>{local.length ? <ComponentVersionTable rows={local} {...table_props} /> : !model.loading ? <p className="about-muted">Local component versions are unavailable.</p> : null}</section>
        <section className="about-section" aria-labelledby="about-remote"><h2 id="about-remote">Connected hosts</h2><p className="about-muted">Versions reported by active remote terminal connections, compared with this app’s build. Last-observed values may have changed.</p>{remote.length ? <ComponentVersionTable rows={remote} reference_label="This app build" {...table_props} /> : !model.loading ? <p className="about-empty">No active remote terminal connections.</p> : null}</section>
        <p className="about-footnote">“Different build” means the source differs; it does not establish which build is newer. Unknown versions cannot be compared.</p>
      </div>
    </section>
    {model.preflight ? createPortal(<QuickInput title={`Restart ${model.preflight.label}`} description={restartDescription(model.preflight)} mode={{ kind: "confirm", confirm_label: "Restart ctld", destructive: true }} onCancel={model.cancelRestart} onSubmit={model.confirmRestart} />, document.body) : null}
  </>;
}
