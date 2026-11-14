import { DEFAULT_PREFIX, TMUX_PREFIX, PREFIX_ACTIONS, prefixActionTitle, resolvePrefix } from "../../features/commands/prefixKeymap";
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
import { localizedErrorText } from "../../lib/errors";
import { useLocale } from "../../features/i18n/LocaleContext";
import type { DisplayText } from "../../features/i18n/messages";

interface Props {
  commands: readonly AppCommand[];
  document: KeybindingsDocument;
  path: string;
  error: DisplayText | null;
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
  const { t, locale, formatText } = useLocale();
  const [selected, setSelected] = useState<string | null>(null);
  const [failure, setFailure] = useState<DisplayText | null>(null);
  const [saving, setSaving] = useState(false);
  const [draft, setDraft] = useState("");
  const busy = useRef(false);
  const bindings = resolveKeymap(document, platform);
  const prefix = resolvePrefix(document, bindings, platform);
  const choices = [
    { id: "preset.ctmux", title: t("shortcuts.ctmux_preset") },
    { id: "preset.tmux", title: t("shortcuts.tmux_preset") },
    { id: "prefix.key", title: t("shortcuts.prefix") },
    ...PREFIX_ACTIONS.map((action) => ({ id: `prefix.${action.id}`, title: t("shortcuts.prefix_action", { v1: prefixActionTitle(action, locale) }) })),
    ...commands.filter((command) =>
      CONFIGURABLE_COMMAND_IDS.includes(command.id),
    ),
    { id: QUICK_INPUT_IDS.accept, title: t("dialog.command_accept") },
    { id: QUICK_INPUT_IDS.cancel, title: t("dialog.command_cancel") },
    { id: QUICK_INPUT_IDS.back, title: t("shortcuts.previous_step") },
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
    } catch (problem) { setFailure(localizedErrorText(problem)); }
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
      setFailure(localizedErrorText(problem));
    } finally {
      busy.current = false;
      setSaving(false);
    }
  }
  const issue = failure ?? error;
  return (
    <QuickInput
      key={selected ?? "commands"}
      title={
        selected
          ? t("shortcuts.edit_title", { v1: choices.find((choice) => choice.id === selected)?.title ?? selected })
          : t("shortcuts.title")
      }
      description={
        selected
          ? selected.startsWith("prefix.")
            ? (selected === "prefix.key" ? t("shortcuts.prefix_help") : t("shortcuts.prefix_action_help"))
            : t("shortcuts.binding_help")
          : [t("shortcuts.choose_help"), path && t("shortcuts.saved_in", { path })].filter(Boolean).join(" ")
      }
      error={issue === null ? null : formatText(issue)}
      cancel_disabled={saving}
      mode={
        saving
          ? { kind: "progress", message: t("shortcuts.saving") }
          : selected
            ? {
                kind: "input",
                label: t("shortcuts.field"),
                initial_value: draft,
                submit_label: t("shortcuts.save"),
              }
            : {
                kind: "pick",
                choices: choices.map((choice) => ({
                  id: choice.id,
                  label: choice.title,
                  detail: choice.id.startsWith("preset.") ? t("shortcuts.reset_prefix") : choice.id === "prefix.key" ? prefix.label ?? t("shortcuts.disabled") : choice.id.startsWith("prefix.") ? prefix.bindings.get(choice.id.slice(7)) ?? t("shortcuts.unbound") : bindings.has(choice.id)
                    ? formatKeybinding(bindings.get(choice.id)!, platform)
                    : t("shortcuts.unbound"),
                })),
              }
      }
      onChange={setDraft}
      onSubmit={(value) => {
        if (value === "preset.ctmux" && !selected) void applyPreset(false);
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
