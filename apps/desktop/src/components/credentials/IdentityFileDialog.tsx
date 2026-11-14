import { useLocale } from "../../features/i18n/LocaleContext";
import { identityName } from "../../features/credentials/identityFiles";
import type { IdentityFile } from "../../lib/types";
import type { DisplayText } from "../../features/i18n/messages";
import { QuickInput } from "../commands/QuickInput";

interface Props {
  file: IdentityFile;
  action: "save" | "forget" | "saving" | "forgetting";
  error?: DisplayText;
  on_save(passphrase: string): Promise<void>;
  on_forget(): Promise<void>;
  on_cancel(): void;
}

export function IdentityFileDialog({ file, action, error, on_save, on_forget, on_cancel }: Props) {
  const { t, formatText } = useLocale();
  const name = identityName(file);
  if (action === "saving" || action === "forgetting") return <QuickInput key={action} title={t(action === "saving" ? "credentials.save_for" : "credentials.forget_for", { name })} description={file.display_path} mode={{ kind: "progress", message: action === "saving" ? t("credentials.saving_passphrase") : t("credentials.forgetting_passphrase"), detail: t("credentials.passphrase_progress_detail") }} cancel_disabled onCancel={on_cancel} onSubmit={() => {}} />;
  if (action === "forget") return <QuickInput key="forget" title={t("credentials.forget_for", { name })} description={t("credentials.forget_passphrase_description", { path: file.display_path })} mode={{ kind: "confirm", confirm_label: t("credentials.forget_passphrase"), destructive: true }} onCancel={on_cancel} onSubmit={on_forget} />;
  const replacing = file.passphrase_state === "saved" || file.passphrase_state === "file_changed";
  const description = file.passphrase_state === "file_changed"
    ? t("credentials.changed_passphrase_description", { path: file.display_path })
    : t(replacing ? "credentials.replace_passphrase_description" : "credentials.save_passphrase_description", { path: file.display_path });
  return <QuickInput key="save" title={t(replacing ? "credentials.replace_for" : "credentials.save_for", { name })} description={description} error={error === undefined ? undefined : formatText(error)} mode={{ kind: "input", label: t("credentials.key_passphrase"), secret: true, submit_label: replacing ? t("credentials.verify_replace") : t("credentials.verify_save") }} onCancel={on_cancel} onSubmit={on_save} />;
}
