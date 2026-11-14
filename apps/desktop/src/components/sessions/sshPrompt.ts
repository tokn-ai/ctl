import { translate, type Locale } from "../../features/i18n/messages";
import type { SshPrompt } from "../../lib/types";
import type { QuickInputMode } from "../commands/QuickInput";

export function promptTitle(prompt: SshPrompt, locale: Locale = "en"): string {
  switch (prompt.kind) {
    case "confirm":
      return translate(locale, "ssh_prompt.verification");
    case "secret":
      return translate(locale, "ssh_prompt.authentication");
    case "credential_save":
      return translate(locale, "ssh_prompt.save");
    case "credential_save_error":
      return translate(locale, "ssh_prompt.save_failed");
  }
}

export function promptMode(prompt: SshPrompt, locale: Locale = "en"): QuickInputMode {
  switch (prompt.kind) {
    case "confirm":
      return { kind: "confirm", confirm_label: translate(locale, "ssh_prompt.trust") };
    case "secret":
      return { kind: "input", label: translate(locale, "ssh_prompt.response"), secret: true };
    case "credential_save":
      return {
        kind: "pick",
        choices: [
          {
            id: "yes",
            label: translate(locale, "ssh_prompt.yes"),
            detail: translate(locale, "ssh_prompt.save_detail"),
          },
          {
            id: "no",
            label: translate(locale, "ssh_prompt.no"),
            detail: translate(locale, "ssh_prompt.skip_detail"),
          },
          {
            id: "never",
            label: translate(locale, "ssh_prompt.never"),
            detail: translate(locale, "ssh_prompt.never_detail"),
          },
        ],
      };
    case "credential_save_error":
      return { kind: "confirm", confirm_label: translate(locale, "ssh_prompt.continue") };
  }
}
