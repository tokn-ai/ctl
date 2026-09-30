import { identityName } from "../../features/credentials/identityFiles";
import type { IdentityFile } from "../../lib/types";
import { QuickInput } from "../commands/QuickInput";

interface Props {
  file: IdentityFile;
  action: "save" | "forget" | "saving" | "forgetting";
  on_save(passphrase: string): Promise<void>;
  on_forget(): Promise<void>;
  on_cancel(): void;
}

export function IdentityFileDialog({ file, action, on_save, on_forget, on_cancel }: Props) {
  const name = identityName(file);
  if (action === "saving" || action === "forgetting") return <QuickInput key={action} title={`${action === "saving" ? "Save" : "Forget"} passphrase for ${name}`} description={file.display_path} mode={{ kind: "progress", message: action === "saving" ? "Verifying the passphrase and saving to Keychain…" : "Removing the saved passphrase from Keychain…", detail: "This operation will finish even if you leave Credentials." }} cancel_disabled onCancel={on_cancel} onSubmit={() => {}} />;
  if (action === "forget") return <QuickInput key="forget" title={`Forget passphrase for ${name}`} description={`Remove only the saved passphrase from Keychain. The identity file ${file.display_path} stays in place. Active connections are unchanged.`} mode={{ kind: "confirm", confirm_label: "Forget passphrase", destructive: true }} onCancel={on_cancel} onSubmit={on_forget} />;
  const replacing = file.passphrase_state === "saved" || file.passphrase_state === "file_changed";
  const description = file.passphrase_state === "file_changed"
    ? `This file has changed. Verify a passphrase for the current file to replace the passphrase saved for the older file. ${file.display_path}`
    : `${replacing ? "Replace the saved passphrase after verifying" : "Verify"} that it unlocks ${file.display_path}. The passphrase is stored in Keychain; the private key stays in its file.`;
  return <QuickInput key="save" title={`${replacing ? "Replace" : "Save"} passphrase for ${name}`} description={description} mode={{ kind: "input", label: "Key passphrase", secret: true, submit_label: replacing ? "Verify and replace" : "Verify and save" }} onCancel={on_cancel} onSubmit={on_save} />;
}
