import { canForgetIdentity, canSaveIdentity, identityName, identityPassphraseLabel } from "../../features/credentials/identityFiles";
import type { IdentityFile } from "../../lib/types";

interface Props {
  identity_files: IdentityFile[];
  keychain_available: boolean;
  disabled: boolean;
  on_save(file: IdentityFile): void;
  on_forget(file: IdentityFile): void;
}

export function IdentityFilesTable({ identity_files, keychain_available, disabled, on_save, on_forget }: Props) {
  return <div className="credentials-table-scroll"><table className="credentials-table identity-files-table" aria-label="Identity files">
    <thead><tr><th scope="col">Identity file</th><th scope="col">Key</th><th scope="col">Used by</th><th scope="col">Passphrase</th><th scope="col"><span className="credentials-sr-only">Actions</span></th></tr></thead>
    <tbody>{identity_files.map((file) => <tr key={file.identity_id}>
      <th scope="row"><span title={[file.path, file.detail].filter(Boolean).join("\n")}><strong>{identityName(file)}</strong><span className="identity-file-path">{file.display_path}</span></span></th>
      <td><span title={[file.key_type, file.fingerprint, file.detail].filter(Boolean).join("\n")}>
        {file.file_state === "ready" ? file.key_type ?? "Not verified" : { missing: "File missing", unreadable: "Cannot read file", unsupported: "Unsupported file" }[file.file_state]}
        {file.fingerprint ? <span className="identity-fingerprint">{file.fingerprint}</span> : null}
      </span></td>
      <td><span title={file.used_by.join("\n") || undefined}>{file.used_by.join(", ") || "Not recorded"}</span></td>
      <td><span title={file.detail ?? undefined}>{!keychain_available && file.passphrase_state !== "not_required" ? "Not checked" : identityPassphraseLabel(file)}</span></td>
      <td className="credentials-row-action">
        {canSaveIdentity(file, keychain_available) ? <button type="button" disabled={disabled} aria-label={`${file.passphrase_state === "saved" || file.passphrase_state === "file_changed" ? "Replace" : "Save"} passphrase for ${identityName(file)}`} onClick={() => on_save(file)}>{file.passphrase_state === "saved" || file.passphrase_state === "file_changed" ? "Replace" : "Save passphrase"}</button> : null}
        {canForgetIdentity(file, keychain_available) ? <button type="button" disabled={disabled} aria-label={`Forget passphrase for ${identityName(file)}`} onClick={() => on_forget(file)}>Forget</button> : null}
      </td>
    </tr>)}</tbody>
  </table></div>;
}
