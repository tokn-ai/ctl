import { DEFAULT_PREFIX, TMUX_PREFIX, PREFIX_ACTIONS, resolvePrefix } from "../../features/commands/prefixKeymap";
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
  const prefix = resolvePrefix(document, bindings, platform);
  const choices = [
    { id: "preset.rmux", title: "Use rmux prefix preset" },
    { id: "preset.tmux", title: "Use tmux-style prefix preset" },
    { id: "prefix.key", title: "Terminal prefix" },
    ...PREFIX_ACTIONS.map((action) => ({ id: `prefix.${action.id}`, title: `Prefix · ${action.title}` })),
    ...commands.filter((command) =>
      CONFIGURABLE_COMMAND_IDS.includes(command.id),
    ),
    { id: QUICK_INPUT_IDS.accept, title: "Accept Quick Input" },
    { id: QUICK_INPUT_IDS.cancel, title: "Cancel Quick Input" },
    { id: QUICK_INPUT_IDS.back, title: "Previous Quick Input Step" },
  ];
  async function applyPreset(tmux: boolean) {
    if (busy.current) return;
    setFailure(null);
    busy.current = true;
    setSaving(true);
    try {
      const next = { ...document, prefix: tmux ? TMUX_PREFIX : DEFAULT_PREFIX };
      resolveKeymap(next, platform);
      await onSave(next);
    } catch (problem) { setFailure(errorMessage(problem)); }
    finally { busy.current = false; setSaving(false); }
  }
  async function save(value: string) {
    if (!selected || busy.current) return;
    setDraft(value);
    setFailure(null);
    try {
      const overrides = document.overrides.filter(
        (entry) => entry.command_id !== selected,
      );
      if (!selected.startsWith("prefix.") && value.trim().toLowerCase() !== "default")
        overrides.push({
          command_id: selected,
          keybinding: parseKeybinding(value),
        });
      let next: KeybindingsDocument = { ...document, overrides };
      if (selected.startsWith("prefix.")) {
        const settings = document.prefix ?? DEFAULT_PREFIX;
        if (selected === "prefix.key") {
          next = { ...document, prefix: { ...settings, key: value.trim().toLowerCase() === "default" ? DEFAULT_PREFIX.key : value.trim() || null } };
        } else {
          const id = selected.slice("prefix.".length);
          const entries = settings.bindings.filter((entry) => entry.command_id !== id);
          if (value.trim().toLowerCase() !== "default") entries.push({ command_id: id, key: value.trim() || null });
          next = { ...document, prefix: { ...settings, bindings: entries } };
        }
      }
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
          ? selected.startsWith("prefix.")
            ? (selected === "prefix.key" ? "Enter Ctrl+letter or Alt+letter. Press it twice to send it to the terminal. Blank disables; default restores Ctrl+B." : "Enter a letter, digit, arrow name, !, :, %, or a double quote. Escape cancels a sequence. Blank disables; default restores the binding.")
            : "Enter Primary+Shift+E, Alt+F2, etc. Primary means Cmd on macOS and Ctrl elsewhere. Blank disables the shortcut; default restores it."
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
                  detail: choice.id.startsWith("preset.") ? "Reset prefix and prefix actions; keep app shortcuts" : choice.id === "prefix.key" ? prefix.label : choice.id.startsWith("prefix.") ? prefix.bindings.get(choice.id.slice(7)) ?? "Unbound" : bindings.has(choice.id)
                    ? formatKeybinding(bindings.get(choice.id)!, platform)
                    : "Unbound",
                })),
              }
      }
      onChange={setDraft}
      onSubmit={(value) => {
        if (value === "preset.rmux" && !selected) void applyPreset(false);
        else if (value === "preset.tmux" && !selected) void applyPreset(true);
        else if (selected) void save(value);
        else {
          setFailure(null);
          setDraft(value === "prefix.key" ? document.prefix?.key ?? (document.prefix ? "" : DEFAULT_PREFIX.key!) : value.startsWith("prefix.") ? prefix.bindings.get(value.slice(7)) ?? "" : editableKeybinding(bindings.get(value)));
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
