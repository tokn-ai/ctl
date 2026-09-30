import type { CredentialRecord } from "../../lib/types";

const kind_labels: Record<CredentialRecord["kind"], string> = {
  ssh_password: "SSH password",
  ssh_key_passphrase: "SSH key passphrase",
  ssh_credential: "Saved SSH credential",
  vpn_password: "VPN password",
  tailscale_sign_in: "Tailscale sign-in",
};

const storage_labels: Record<CredentialRecord["storage"], string> = {
  keychain: "Keychain",
  vpn_settings: "VPN settings",
  container_volume: "Container volume",
};

function dateLabel(value: number | null): string {
  return value === null ? "Not recorded" : new Date(value).toLocaleString();
}

interface Props {
  credentials: CredentialRecord[];
  busy_id: string | null;
  on_forget(credential: CredentialRecord): void;
  on_manage_vpn(connection_id: string | null): void;
}

export function CredentialTable({ credentials, busy_id, on_forget, on_manage_vpn }: Props) {
  return <div className="credentials-table-scroll"><table className="credentials-table">
    <thead><tr><th scope="col">Name</th><th scope="col">Type</th><th scope="col">Account / target</th><th scope="col">Storage</th><th scope="col">Updated</th><th scope="col"><span className="credentials-sr-only">Actions</span></th></tr></thead>
    <tbody>{credentials.map((credential) => <tr key={credential.credential_id}>
      <th scope="row"><span title={[credential.name, credential.detail].filter(Boolean).join("\n")}>{credential.name}</span></th>
      <td><span title={credential.detail ?? undefined}>{kind_labels[credential.kind]}</span></td>
      <td><span title={[credential.account, credential.target].filter(Boolean).join(" · ") || undefined}>{[credential.account, credential.target].filter(Boolean).join(" · ") || "Not recorded"}</span></td>
      <td><span title={credential.storage === "vpn_settings" ? "Saved in the private VPN settings file." : credential.storage === "container_volume" ? "Sign-in data is owned by Tailscale. Its presence has not been verified." : "Password or passphrase stored in macOS Keychain."}>{storage_labels[credential.storage]}</span></td>
      <td><span title={`Created: ${dateLabel(credential.created_at_ms)}\nUpdated: ${dateLabel(credential.updated_at_ms)}`}>{credential.updated_at_ms === null ? "Not recorded" : new Date(credential.updated_at_ms).toLocaleDateString()}</span></td>
      <td className="credentials-row-action">{credential.storage === "keychain" && credential.action === "forget"
        ? <button type="button" disabled={busy_id !== null} aria-label={`Forget ${credential.name}`} onClick={() => on_forget(credential)}>{busy_id === credential.credential_id ? "Forgetting…" : "Forget"}</button>
        : <button type="button" disabled={busy_id !== null} aria-label={`Manage ${credential.name}`} onClick={() => on_manage_vpn(credential.vpn_connection_id)}>Manage</button>}</td>
    </tr>)}</tbody>
  </table></div>;
}
