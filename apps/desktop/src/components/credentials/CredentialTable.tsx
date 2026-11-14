import { useLocale } from "../../features/i18n/LocaleContext";
import type { CredentialRecord } from "../../lib/types";

const kind_labels: Record<CredentialRecord["kind"], string> = {
  ssh_password: "credentials.ssh_password",
  ssh_key_passphrase: "credentials.ssh_key_passphrase",
  ssh_credential: "credentials.ssh_credential",
  vpn_password: "credentials.vpn_password",
  tailscale_sign_in: "credentials.tailscale_sign_in",
};

const storage_labels: Record<CredentialRecord["storage"], string> = {
  keychain: "credentials.keychain",
  vpn_settings: "credentials.vpn_settings",
  container_volume: "credentials.container_volume",
};

interface Props {
  credentials: CredentialRecord[];
  busy_id: string | null;
  on_forget(credential: CredentialRecord): void;
  on_manage_vpn(connection_id: string | null): void;
}

export function CredentialTable({ credentials, busy_id, on_forget, on_manage_vpn }: Props) {
  const { t, locale } = useLocale();
  const dateLabel = (value: number | null) => value === null ? t("credentials.not_recorded") : new Date(value).toLocaleString(locale);
  return <div className="credentials-table-scroll"><table className="credentials-table">
    <thead><tr><th scope="col">{t("credentials.name")}</th><th scope="col">{t("credentials.type")}</th><th scope="col">{t("credentials.account_target")}</th><th scope="col">{t("credentials.storage")}</th><th scope="col">{t("credentials.updated")}</th><th scope="col"><span className="credentials-sr-only">{t("credentials.actions")}</span></th></tr></thead>
    <tbody>{credentials.map((credential) => <tr key={credential.credential_id}>
      <th scope="row"><span title={[credential.name, credential.detail].filter(Boolean).join("\n")}>{credential.name}</span></th>
      <td><span title={credential.detail ?? undefined}>{t(kind_labels[credential.kind])}</span></td>
      <td><span title={[credential.account, credential.target].filter(Boolean).join(" · ") || undefined}>{[credential.account, credential.target].filter(Boolean).join(" · ") || t("credentials.not_recorded")}</span></td>
      <td><span title={credential.storage === "vpn_settings" ? t("credentials.vpn_storage_detail") : credential.storage === "container_volume" ? t("credentials.tailscale_storage_detail") : t("credentials.keychain_storage_detail")}>{t(storage_labels[credential.storage])}</span></td>
      <td><span title={t("credentials.date_tooltip", { created: dateLabel(credential.created_at_ms), updated: dateLabel(credential.updated_at_ms) })}>{credential.updated_at_ms === null ? t("credentials.not_recorded") : new Date(credential.updated_at_ms).toLocaleDateString(locale)}</span></td>
      <td className="credentials-row-action">{credential.storage === "keychain" && credential.action === "forget"
        ? <button type="button" disabled={busy_id !== null} aria-label={t("credentials.forget_named", { name: credential.name })} onClick={() => on_forget(credential)}>{busy_id === credential.credential_id ? t("credentials.forgetting") : t("credentials.forget")}</button>
        : <button type="button" disabled={busy_id !== null} aria-label={t("credentials.manage_named", { name: credential.name })} onClick={() => on_manage_vpn(credential.vpn_connection_id)}>{t("credentials.manage")}</button>}</td>
    </tr>)}</tbody>
  </table></div>;
}
