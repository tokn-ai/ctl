import { useLocale } from "../features/i18n/LocaleContext";
import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { QuickInput } from "../components/commands/QuickInput";
import { IdentityFilesTable } from "../components/credentials/IdentityFilesTable";
import { IdentityFileDialog } from "../components/credentials/IdentityFileDialog";
import { CredentialTable } from "../components/credentials/CredentialTable";
import { Icon } from "../components/ui/Icon";
import { InlineFeedback } from "../components/feedback/InlineFeedback";
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

function sourceMessage(source: CredentialSourceStatus, t: ReturnType<typeof useLocale>["t"]): string {
  const name = t(source.source === "keychain" ? "credentials.keychain" : "credentials.vpn_credentials");
  if (source.message) return t("credentials.source_detail", { name, detail: source.message });
  return {
    ready: t("credentials.source_checked", { name }),
    partial: t(source.source === "keychain" ? "credentials.partial_keychain" : "credentials.partial_vpn"),
    unavailable: t("credentials.source_unavailable", { name }),
    unsupported: t("credentials.source_unsupported", { name }),
  }[source.state];
}

export function CredentialsPage({ visible, targets, on_close, on_dialog_change, on_manage_vpn }: Props) {
  const { t, formatText, locale } = useLocale();
  const model = useCredentials(visible, targets);
  const heading = useRef<HTMLHeadingElement>(null);
  const has_dialog = visible && (model.pending !== null || model.identity_dialog !== null || model.metadata_importing);
  const interactions_disabled = model.busy_id !== null || has_dialog;
  useEffect(() => { if (visible) heading.current?.focus(); }, [visible]);
  useEffect(() => {
    on_dialog_change(has_dialog);
    return () => on_dialog_change(false);
  }, [has_dialog, on_dialog_change]);
  const incomplete = model.snapshot?.sources.filter((source) => source.state !== "ready") ?? [];
  const has_available_source = model.snapshot?.sources.some((source) => source.state === "ready" || source.state === "partial") ?? false;
  const empty_message = model.snapshot && model.snapshot.metadata_import_required !== false ? t("credentials.all_empty_import")
    : !has_available_source ? t("credentials.all_empty_unavailable")
    : incomplete.length ? t("credentials.all_empty_partial") : t("credentials.all_empty");
  const identity_warning = model.identities?.warning ?? (model.identities && !model.identities.complete
    && model.identities.metadata_import_required === false && model.identities.keychain_available
    && !model.identities.keychain_message ? t("credentials.identity_partial") : null);
  const import_feedback = model.import_feedback
    ? <InlineFeedback severity={model.import_feedback.severity}>{formatText(model.import_feedback.message)}</InlineFeedback> : null;

  return <>
    <section className="credentials-page" aria-label={t("credentials.title")} hidden={!visible}>
      <header className="credentials-page-header">
        <div><h1 ref={heading} tabIndex={-1}>{t("credentials.title")}</h1><p>{t("credentials.intro")}</p></div>
        <button type="button" onClick={on_close}><Icon name="close" />{t("credentials.back")}</button>
      </header>
      <div className="credentials-content">
        <div className="credentials-refresh-row"><p>{model.snapshot ? t("credentials.last_checked", { time: new Date(model.snapshot.checked_at_ms).toLocaleTimeString(locale) }) : t("credentials.unchecked")}</p><button type="button" disabled={model.loading || interactions_disabled} onClick={() => void model.refresh()}><Icon name="refresh" />{model.loading ? t("credentials.checking") : t("credentials.refresh")}</button></div>
        <div className="credentials-import-area" role="region" aria-label={model.metadata_import_required ? t("credentials.import_region") : t("credentials.reimport_region")}>
          {model.metadata_import_required ? <div className="credentials-import">
            <p id="credential-import-description">{t("credentials.import_description")}</p>
            <button type="button" aria-describedby="credential-import-description" disabled={model.loading || interactions_disabled} onClick={() => void model.importMetadata()}>{model.metadata_importing ? t("credentials.importing") : t("credentials.import")}</button>
          </div> : <div className="credentials-reimport">
            <button type="button" aria-describedby="credential-reimport-description" disabled={model.loading || interactions_disabled} onClick={() => void model.importMetadata()}>{model.metadata_importing ? t("credentials.importing") : t("credentials.reimport")}</button>
            <p id="credential-reimport-description">{t("credentials.reimport_description")}</p>
          </div>}
          {import_feedback}
        </div>
        <section className="credentials-section" aria-label={t("credentials.identity_inventory")}>
          <h2>{t("credentials.identity_files")}</h2>
          {model.identity_error ? <InlineFeedback>{t(model.identities ? "credentials.identity_refresh_stale" : "credentials.identity_refresh_failed", { error: model.identity_error })}</InlineFeedback> : null}
          {identity_warning ? <InlineFeedback severity="warning">{identity_warning}</InlineFeedback> : null}
          {model.identities?.keychain_message ? <InlineFeedback severity="warning">{model.identities.keychain_message}</InlineFeedback>
            : model.identities && !model.identities.keychain_available ? <InlineFeedback severity="warning">{t("credentials.identity_keychain_check")}</InlineFeedback> : null}
          {model.loading && !model.identities ? <p role="status">{t("credentials.checking_identities")}</p> : null}
          {model.identities?.identity_files.length ? <IdentityFilesTable identity_files={model.identities.identity_files} metadata_import_required={model.identities.metadata_import_required !== false} keychain_available={model.identities.keychain_available && !model.identity_error} disabled={interactions_disabled} on_save={model.requestIdentitySave} on_forget={model.requestIdentityForget} />
            : model.identities && !model.loading ? <p className="credentials-empty">{model.identities.metadata_import_required !== false ? t("credentials.identity_empty_import") : model.identities.complete ? t("credentials.identity_empty") : t("credentials.identity_empty_checked")}</p> : null}
        </section>
        <section className="credentials-section" aria-label={t("credentials.saved_inventory")}>
          <h2>{t("credentials.saved_credentials")}</h2>
          {model.error ? <InlineFeedback>{t(model.snapshot ? "credentials.refresh_stale" : "credentials.refresh_failed", { error: model.error })}</InlineFeedback> : null}
          {incomplete.map((source) => <InlineFeedback key={source.source} severity="warning">{sourceMessage(source, t)}</InlineFeedback>)}
          {model.loading && !model.snapshot ? <p role="status">{t("credentials.checking_saved")}</p> : null}
          {model.snapshot?.credentials.length ? <CredentialTable credentials={model.snapshot.credentials} busy_id={model.busy_id ?? (has_dialog ? "dialog" : null)} on_forget={model.requestForget} on_manage_vpn={on_manage_vpn} />
            : model.snapshot && !model.loading ? <p className="credentials-empty">{empty_message}</p> : null}
        </section>
        <p className="credentials-footnote">{t("credentials.footnote")}</p>
      </div>
    </section>
    {visible && model.pending ? createPortal(<QuickInput title={t("credentials.forget_named", { name: model.pending.name })} description={t("credentials.forget_description")} mode={{ kind: "confirm", confirm_label: t("credentials.forget_credential"), destructive: true }} onCancel={model.cancelForget} onSubmit={model.confirmForget} />, document.body) : null}
    {visible && model.metadata_importing ? createPortal(<QuickInput title={t("credentials.import")} description={t("credentials.import_progress_description")} mode={{ kind: "progress", message: t("credentials.import_authorize"), detail: t("credentials.import_detail") }} cancel_disabled onCancel={() => {}} onSubmit={() => {}} />, document.body) : null}
    {visible && model.identity_dialog ? createPortal(<IdentityFileDialog {...model.identity_dialog} on_save={model.confirmIdentitySave} on_forget={model.confirmIdentityForget} on_cancel={model.cancelDialog} />, document.body) : null}
  </>;
}
