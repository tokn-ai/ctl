import type { SshPrompt } from "../../lib/types";
import type { QuickInputMode } from "../commands/QuickInput";

export function promptTitle(prompt: SshPrompt): string {
  switch (prompt.kind) {
    case "confirm":
      return "SSH host verification";
    case "secret":
      return "SSH authentication";
    case "credential_save":
      return "Save SSH credential?";
    case "credential_save_error":
      return "Credential not saved";
  }
}

export function promptMode(prompt: SshPrompt): QuickInputMode {
  switch (prompt.kind) {
    case "confirm":
      return { kind: "confirm", confirm_label: "Trust and connect" };
    case "secret":
      return { kind: "input", label: "SSH response", secret: true };
    case "credential_save":
      return {
        kind: "pick",
        choices: [
          {
            id: "yes",
            label: "Yes",
            detail: "Save in Keychain and require Touch ID for future access.",
          },
          {
            id: "no",
            label: "No",
            detail: "Do not save this time; ask again after a future authentication.",
          },
          {
            id: "never",
            label: "Never",
            detail: "Never offer to save credentials for this SSH host.",
          },
        ],
      };
    case "credential_save_error":
      return { kind: "confirm", confirm_label: "Continue" };
  }
}
