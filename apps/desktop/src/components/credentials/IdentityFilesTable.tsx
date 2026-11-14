import { useLocale } from "../../features/i18n/LocaleContext";
import { canForgetIdentity, canSaveIdentity, identityName, identityPassphraseMessage } from "../../features/credentials/identityFiles";
import type { IdentityFile } from "../../lib/types";

interface Props {
  identity_files: IdentityFile[];
  keychain_available: boolean;
  metadata_import_required: boolean;
  disabled: boolean;
  on_save(file: IdentityFile): void;
  on_forget(file: IdentityFile): void;
}

export function IdentityFilesTable({ identity_files, keychain_available, metadata_import_required, disabled, on_save, on_forget }: Props) {
  const { t, intl } = useLocale();
  return <div className="credentials-table-scroll"><table className="credentials-table identity-files-table" aria-label={t("credentials.identity_files")}>
    <thead><tr><th scope="col">{t("credentials.identity_file")}</th><th scope="col">{t("credentials.key")}</th><th scope="col">{t("credentials.used_by")}</th><th scope="col">{t("credentials.passphrase")}</th><th scope="col"><span className="credentials-sr-only">{t("credentials.actions")}</span></th></tr></thead>
    <tbody>{identity_files.map((file) => <tr key={file.identity_id}>
      <th scope="row"><span title={[file.path, file.detail].filter(Boolean).join("\n")}><strong>{identityName(file)}</strong><span className="identity-file-path">{file.display_path}</span></span></th>
      <td><span title={[file.key_type, file.fingerprint, file.detail].filter(Boolean).join("\n")}>
        {file.file_state === "ready" ? file.key_type ?? t("credentials.not_verified") : { missing: t("credentials.file_missing"), unreadable: t("credentials.file_unreadable"), unsupported: t("credentials.file_unsupported") }[file.file_state]}
        {file.fingerprint ? <span className="identity-fingerprint">{file.fingerprint}</span> : null}
      </span></td>
      <td><span title={file.used_by.join("\n") || undefined}>{intl.formatList(file.used_by) || t("credentials.not_recorded")}</span></td>
      <td><span title={file.detail ?? undefined}>{(!keychain_available && file.passphrase_state !== "not_required") || (metadata_import_required && file.passphrase_state === "not_saved") ? t("credentials.not_checked") : t(identityPassphraseMessage(file).id)}</span></td>
      <td className="credentials-row-action">
        {canSaveIdentity(file, keychain_available) ? <button type="button" disabled={disabled} aria-label={t(file.passphrase_state === "saved" || file.passphrase_state === "file_changed" ? "credentials.replace_for" : "credentials.save_for", { name: identityName(file) })} onClick={() => on_save(file)}>{file.passphrase_state === "saved" || file.passphrase_state === "file_changed" ? t("credentials.replace") : t("credentials.save_passphrase")}</button> : null}
        {canForgetIdentity(file, keychain_available) ? <button type="button" disabled={disabled} aria-label={t("credentials.forget_for", { name: identityName(file) })} onClick={() => on_forget(file)}>{t("credentials.forget")}</button> : null}
      </td>
    </tr>)}</tbody>
  </table></div>;
}
