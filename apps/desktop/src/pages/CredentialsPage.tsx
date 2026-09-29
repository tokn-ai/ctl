import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { QuickInput } from "../components/commands/QuickInput";
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
  const has_dialog = model.pending !== null;
  useEffect(() => { if (visible) heading.current?.focus(); }, [visible]);
  useEffect(() => { on_dialog_change(has_dialog); }, [has_dialog, on_dialog_change]);
  const incomplete = model.snapshot?.sources.filter((source) => source.state !== "ready") ?? [];
  const has_available_source = model.snapshot?.sources.some((source) => source.state === "ready" || source.state === "partial") ?? false;
  const empty_message = !has_available_source ? "Saved credentials could not be checked."
    : incomplete.length ? "No credentials found in available storage." : "No saved credentials found.";

  return <>
    <section className="credentials-page" aria-label="Credentials" hidden={!visible}>
      <header className="credentials-page-header">
        <div><h1 ref={heading} tabIndex={-1}>Credentials</h1><p>Saved by rmux. Names and metadata only.</p></div>
        <button type="button" onClick={on_close}><Icon name="close" />Back to workspace</button>
      </header>
      <div className="credentials-content">
        <div className="credentials-refresh-row"><p>{model.snapshot ? `Last checked ${new Date(model.snapshot.checked_at_ms).toLocaleTimeString()}` : "Saved credentials have not been checked."}</p><button type="button" disabled={model.loading || model.busy_id !== null} onClick={() => void model.refresh()}><Icon name="refresh" />{model.loading ? "Checking…" : "Refresh"}</button></div>
        {model.error ? <p className="credentials-error" role="alert">Could not refresh credentials: {model.error}{model.snapshot ? " Showing the last successful check." : ""}</p> : null}
        {incomplete.map((source) => <p key={source.source} className="credentials-source-status" role="status">{sourceMessage(source)}</p>)}
        {model.action_error ? <p className="credentials-error" role="alert">{model.action_error}</p> : null}
        {model.notice ? <p className="credentials-notice" role="status">{model.notice}</p> : null}
        {model.loading && !model.snapshot ? <p role="status">Checking saved credentials…</p> : null}
        {model.snapshot?.credentials.length ? <CredentialTable credentials={model.snapshot.credentials} busy_id={model.busy_id} on_forget={model.requestForget} on_manage_vpn={on_manage_vpn} />
          : model.snapshot && !model.loading ? <p className="credentials-empty">{empty_message}</p> : null}
        <p className="credentials-footnote">Keychain contains saved SSH passwords and key passphrases. VPN passwords use private VPN settings; Tailscale sign-in belongs to its container volume. Tailscale entries identify saved profiles; sign-in data has not been verified.</p>
      </div>
    </section>
    {model.pending ? createPortal(<QuickInput title={`Forget ${model.pending.name}`} description="Remove this saved credential from Keychain. Active connections stay connected. A future connection may ask for the password or passphrase again." mode={{ kind: "confirm", confirm_label: "Forget credential", destructive: true }} onCancel={model.cancelForget} onSubmit={model.confirmForget} />, document.body) : null}
  </>;
}
