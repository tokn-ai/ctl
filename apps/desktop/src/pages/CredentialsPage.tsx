import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { QuickInput } from "../components/commands/QuickInput";
import { IdentityFilesTable } from "../components/credentials/IdentityFilesTable";
import { IdentityFileDialog } from "../components/credentials/IdentityFileDialog";
import { CredentialTable } from "../components/credentials/CredentialTable";
import { Icon } from "../components/ui/Icon";
import { useCredentials } from "../features/credentials/useCredentials";
import type { CredentialSourceStatus, CredentialTarget } from "../lib/types";
import "../components/credentials/credentials.css";

interface Props {
  visible: boolean;
  targets: CredentialTarget[];
  on_close(): void;
  on_dialog_change(open: boolean): void;
  on_manage_vpn(connection_id: string | null): void;
}

function sourceMessage(source: CredentialSourceStatus): string {
  const name = source.source === "keychain" ? "Keychain" : "VPN credentials";
  if (source.message) return `${name}: ${source.message}`;
  return {
    ready: `${name} checked.`,
    partial: `Some ${name === "Keychain" ? "Keychain items" : "VPN credentials"} could not be listed.`,
    unavailable: `${name} could not be checked.`,
    unsupported: `${name} cannot be listed on this platform.`,
  }[source.state];
}

export function CredentialsPage({ visible, targets, on_close, on_dialog_change, on_manage_vpn }: Props) {
  const model = useCredentials(visible, targets);
  const heading = useRef<HTMLHeadingElement>(null);
  const has_dialog = visible && (model.pending !== null || model.identity_dialog !== null);
  const interactions_disabled = model.busy_id !== null || has_dialog;
  useEffect(() => { if (visible) heading.current?.focus(); }, [visible]);
  useEffect(() => {
    on_dialog_change(has_dialog);
    return () => on_dialog_change(false);
  }, [has_dialog, on_dialog_change]);
  const incomplete = model.snapshot?.sources.filter((source) => source.state !== "ready") ?? [];
  const has_available_source = model.snapshot?.sources.some((source) => source.state === "ready" || source.state === "partial") ?? false;
  const empty_message = !has_available_source ? "Saved credentials could not be checked."
    : incomplete.length ? "No credentials found in available storage." : "No saved credentials found.";

  return <>
    <section className="credentials-page" aria-label="Credentials" hidden={!visible}>
      <header className="credentials-page-header">
        <div><h1 ref={heading} tabIndex={-1}>Credentials</h1><p>Identity files and saved credentials. Names and metadata only.</p></div>
        <button type="button" onClick={on_close}><Icon name="close" />Back to workspace</button>
      </header>
      <div className="credentials-content">
        <div className="credentials-refresh-row"><p>{model.snapshot ? `Last checked ${new Date(model.snapshot.checked_at_ms).toLocaleTimeString()}` : "Saved credentials have not been checked."}</p><button type="button" disabled={model.loading || interactions_disabled} onClick={() => void model.refresh()}><Icon name="refresh" />{model.loading ? "Checking…" : "Refresh"}</button></div>
        {model.error ? <p className="credentials-error" role="alert">Could not refresh credentials: {model.error}{model.snapshot ? " Showing the last successful check." : ""}</p> : null}
        {incomplete.map((source) => <p key={source.source} className="credentials-source-status" role="status">{sourceMessage(source)}</p>)}
        {model.action_error ? <p className="credentials-error" role="alert">{model.action_error}</p> : null}
        {model.notice ? <p className="credentials-notice" role="status">{model.notice}</p> : null}
        <section className="credentials-section" aria-label="Identity file inventory">
          <h2>Identity files</h2>
          {model.identity_error ? <p className="credentials-error" role="alert">Could not refresh identity files: {model.identity_error}{model.identities ? " Showing the last successful check." : ""}</p> : null}
          {model.identities?.warning || (model.identities && !model.identities.complete) ? <p className="credentials-source-status" role="status">{model.identities.warning ?? "Some identity files could not be checked."}</p> : null}
          {model.identities && !model.identities.keychain_available ? <p className="credentials-source-status" role="status">Keychain passphrase storage could not be checked. Save and Forget are unavailable.</p> : null}
          {model.loading && !model.identities ? <p role="status">Checking identity files…</p> : null}
          {model.identities?.identity_files.length ? <IdentityFilesTable identity_files={model.identities.identity_files} keychain_available={model.identities.keychain_available && !model.identity_error} disabled={interactions_disabled} on_save={model.requestIdentitySave} on_forget={model.requestIdentityForget} />
            : model.identities && !model.loading ? <p className="credentials-empty">{model.identities.complete ? "No identity files found." : "No identity files found in the locations checked."}</p> : null}
        </section>
        <section className="credentials-section" aria-label="Saved credential inventory">
          <h2>Saved credentials</h2>
          {model.loading && !model.snapshot ? <p role="status">Checking saved credentials…</p> : null}
          {model.snapshot?.credentials.length ? <CredentialTable credentials={model.snapshot.credentials} busy_id={model.busy_id ?? (has_dialog ? "dialog" : null)} on_forget={model.requestForget} on_manage_vpn={on_manage_vpn} />
            : model.snapshot && !model.loading ? <p className="credentials-empty">{empty_message}</p> : null}
        </section>
        <p className="credentials-footnote">Identity files include configured paths and keys discovered in ~/.ssh. “Used by” lists recorded host references. Keychain contains saved SSH passwords and key passphrases. VPN passwords use private VPN settings; Tailscale sign-in belongs to its container volume. Tailscale entries identify saved profiles; sign-in data has not been verified.</p>
      </div>
    </section>
    {visible && model.pending ? createPortal(<QuickInput title={`Forget ${model.pending.name}`} description="Remove this saved credential from Keychain. Active connections stay connected. A future connection may ask for the password or passphrase again." mode={{ kind: "confirm", confirm_label: "Forget credential", destructive: true }} onCancel={model.cancelForget} onSubmit={model.confirmForget} />, document.body) : null}
    {visible && model.identity_dialog ? createPortal(<IdentityFileDialog {...model.identity_dialog} on_save={model.confirmIdentitySave} on_forget={model.confirmIdentityForget} on_cancel={model.cancelDialog} />, document.body) : null}
  </>;
}
