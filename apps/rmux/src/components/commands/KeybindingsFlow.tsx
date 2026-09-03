import { useRef, useState } from "react";
import { QuickInput } from "./QuickInput";
import type {
  AppCommand,
  ShortcutPlatform,
} from "../../features/commands/types";
import type { KeybindingsDocument } from "../../lib/types";
import {
  CONFIGURABLE_COMMAND_IDS,
  QUICK_INPUT_IDS,
} from "../../features/commands/commandIds";
import {
  editableKeybinding,
  parseKeybinding,
  resolveKeymap,
} from "../../features/commands/keymap";
import { formatKeybinding } from "../../features/commands/keybindings";
import { errorMessage } from "../../lib/errors";

interface Props {
  commands: readonly AppCommand[];
  document: KeybindingsDocument;
  path: string;
  error: string | null;
  platform: ShortcutPlatform;
  onSave(document: KeybindingsDocument): Promise<void>;
  onClose(): void;
}

export function KeybindingsFlow({
  commands,
  document,
  path,
  error,
  platform,
  onSave,
  onClose,
}: Props) {
  const [selected, setSelected] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [draft, setDraft] = useState("");
  const busy = useRef(false);
  const bindings = resolveKeymap(document, platform);
  const choices = [
    ...commands.filter((command) =>
      CONFIGURABLE_COMMAND_IDS.includes(command.id),
    ),
    { id: QUICK_INPUT_IDS.accept, title: "Accept Quick Input" },
    { id: QUICK_INPUT_IDS.cancel, title: "Cancel Quick Input" },
    { id: QUICK_INPUT_IDS.back, title: "Previous Quick Input Step" },
  ];
  async function save(value: string) {
    if (!selected || busy.current) return;
    setDraft(value);
    setFailure(null);
    try {
      const overrides = document.overrides.filter(
        (entry) => entry.command_id !== selected,
      );
      if (value.trim().toLowerCase() !== "default")
        overrides.push({
          command_id: selected,
          keybinding: parseKeybinding(value),
        });
      const next: KeybindingsDocument = { schema_version: 1, overrides };
      resolveKeymap(next, platform);
      busy.current = true;
      setSaving(true);
      await onSave(next);
      setSelected(null);
    } catch (problem) {
      setFailure(errorMessage(problem));
    } finally {
      busy.current = false;
      setSaving(false);
    }
  }
  return (
    <QuickInput
      key={selected ?? "commands"}
      title={
        selected
          ? `Keyboard shortcut — ${choices.find((choice) => choice.id === selected)?.title ?? selected}`
          : "Keyboard shortcuts"
      }
      description={
        selected
          ? "Enter Primary+Shift+E, Alt+F2, etc. Primary means Cmd on macOS and Ctrl elsewhere. Blank disables the shortcut; default restores it."
          : `Choose an app command to change its shortcut. Native editing/window shortcuts are reserved.${path ? ` Saved in ${path}.` : ""}`
      }
      error={failure ?? error}
      cancel_disabled={saving}
      mode={
        saving
          ? { kind: "progress", message: "Saving keyboard shortcuts…" }
          : selected
            ? {
                kind: "input",
                label: "Shortcut",
                initial_value: draft,
                submit_label: "Save shortcut",
              }
            : {
                kind: "pick",
                choices: choices.map((choice) => ({
                  id: choice.id,
                  label: choice.title,
                  detail: bindings.has(choice.id)
                    ? formatKeybinding(bindings.get(choice.id)!, platform)
                    : "Unbound",
                })),
              }
      }
      onChange={setDraft}
      onSubmit={(value) => {
        if (selected) void save(value);
        else {
          setFailure(null);
          setDraft(editableKeybinding(bindings.get(value)));
          setSelected(value);
        }
      }}
      onBack={
        selected && !saving
          ? () => {
              setSelected(null);
              setFailure(null);
            }
          : undefined
      }
      onCancel={() => {
        if (!saving) onClose();
      }}
    />
  );
}
