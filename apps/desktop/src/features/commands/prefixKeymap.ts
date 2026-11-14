import { LocalizedError, translate, type Locale } from "../i18n/messages";
import type { KeybindingsDocument, TerminalPrefixSettings } from "../../lib/types";
import type { Keybinding, ShortcutPlatform } from "./types";
import { matchesKeybinding } from "./keybindings";

export const PREFIX_ACTIONS = [
  { id: "pane.split_right", title_id: "prefix_action.pane.split_right", key: "V" },
  { id: "pane.split_below", title_id: "prefix_action.pane.split_below", key: "S" },
  { id: "pane.focus_left", title_id: "prefix_action.pane.focus_left", key: "Left" },
  { id: "pane.focus_right", title_id: "prefix_action.pane.focus_right", key: "Right" },
  { id: "pane.focus_up", title_id: "prefix_action.pane.focus_up", key: "Up" },
  { id: "pane.focus_down", title_id: "prefix_action.pane.focus_down", key: "Down" },
  { id: "pane.resize_left", title_id: "prefix_action.pane.resize_left", key: "Ctrl+Left" },
  { id: "pane.resize_right", title_id: "prefix_action.pane.resize_right", key: "Ctrl+Right" },
  { id: "pane.resize_up", title_id: "prefix_action.pane.resize_up", key: "Ctrl+Up" },
  { id: "pane.resize_down", title_id: "prefix_action.pane.resize_down", key: "Ctrl+Down" },
  { id: "pane.resize_left_large", title_id: "prefix_action.pane.resize_left_large", key: "Alt+Left" },
  { id: "pane.resize_right_large", title_id: "prefix_action.pane.resize_right_large", key: "Alt+Right" },
  { id: "pane.resize_up_large", title_id: "prefix_action.pane.resize_up_large", key: "Alt+Up" },
  { id: "pane.resize_down_large", title_id: "prefix_action.pane.resize_down_large", key: "Alt+Down" },
  { id: "pane.zoom", title_id: "prefix_action.pane.zoom", key: "Z" },
  { id: "pane.move", title_id: "prefix_action.pane.move", key: "M" },
  { id: "pane.move_left", title_id: "prefix_action.pane.move_left", key: "Left" },
  { id: "pane.move_right", title_id: "prefix_action.pane.move_right", key: "Right" },
  { id: "pane.move_up", title_id: "prefix_action.pane.move_up", key: "Up" },
  { id: "pane.move_down", title_id: "prefix_action.pane.move_down", key: "Down" },
  { id: "pane.promote", title_id: "prefix_action.pane.promote", key: "!" },
  { id: "tab.next", title_id: "prefix_action.tab.next", key: "N" },
  { id: "tab.previous", title_id: "prefix_action.tab.previous", key: "P" },
  { id: "tab.new_shell_here", title_id: "prefix_action.tab.new_shell_here", key: "C" },
  { id: "view.show_command_palette", title_id: "prefix_action.view.show_command_palette", key: ":" },
] as const;

export function prefixActionTitle(action: (typeof PREFIX_ACTIONS)[number], locale: Locale) {
  return translate(locale, action.title_id);
}

export const TMUX_PREFIX: TerminalPrefixSettings = { key: "Ctrl+B", bindings: [
  { command_id: "pane.split_right", key: "%" },
  { command_id: "pane.split_below", key: '"' },
] };

export const DEFAULT_PREFIX: TerminalPrefixSettings = { key: "Ctrl+B", bindings: [] };
const arrows: Record<string, string> = { left: "ArrowLeft", right: "ArrowRight", up: "ArrowUp", down: "ArrowDown" };

export function prefixActionMode(id: string) {
  return id.startsWith("pane.move_") ? "move" : "prefix";
}

export function prefixStroke(text: string) {
  const match = /^(Ctrl|Alt)\+([a-z])$/i.exec(text.trim());
  if (!match) throw new LocalizedError("shortcuts.invalid_prefix");
  const control = match[1].toLowerCase() === "ctrl";
  const letter = match[2].toUpperCase();
  return { code: `Key${letter}`, ctrlKey: control, altKey: !control, metaKey: false, shiftKey: false,
    bytes: new Uint8Array(control ? [letter.charCodeAt(0) - 64] : [27, letter.toLowerCase().charCodeAt(0)]) };
}

export function prefixActionKey(text: string): string {
  const value = text.trim();
  const modified = /^(Ctrl|Alt)\+(Left|Right|Up|Down)$/i.exec(value);
  if (modified) return `${modified[1].toLowerCase() === "ctrl" ? "Ctrl" : "Alt"}+${arrows[modified[2].toLowerCase()]}`;
  if (/^[a-z0-9]$/i.test(value)) return value.toLowerCase();
  if (["!", ":", "%", '"'].includes(value)) return value;
  if (Object.prototype.hasOwnProperty.call(arrows, value.toLowerCase())) return arrows[value.toLowerCase()];
  throw new LocalizedError("shortcuts.invalid_prefix_action");
}

export function prefixEventKey(event: { key: string; ctrlKey: boolean; altKey: boolean; metaKey: boolean; shiftKey: boolean }): string | null {
  if (event.metaKey || event.ctrlKey && event.altKey) return null;
  if (event.ctrlKey || event.altKey) {
    if (event.shiftKey || !Object.values(arrows).includes(event.key)) return null;
    return `${event.ctrlKey ? "Ctrl" : "Alt"}+${event.key}`;
  }
  if (event.shiftKey && event.key.length !== 1) return null;
  return event.key.length === 1 ? event.key.toLowerCase() : event.key;
}

export function resolvePrefix(document: KeybindingsDocument, direct: ReadonlyMap<string, Keybinding>, platform: ShortcutPlatform) {
  const settings = document.prefix ?? DEFAULT_PREFIX;
  let stroke = settings.key === null ? null : prefixStroke(settings.key);
  if (!Array.isArray(settings.bindings) || settings.bindings.length > PREFIX_ACTIONS.length)
    throw new LocalizedError("shortcuts.invalid_prefix_bindings");
  const bindings = new Map<string, string>(PREFIX_ACTIONS.map((action) => [action.id, action.key]));
  const seen = new Set<string>();
  for (const entry of settings.bindings) {
    if (!bindings.has(entry.command_id) || seen.has(entry.command_id)) throw new LocalizedError("shortcuts.unknown_prefix_action");
    seen.add(entry.command_id);
    if (entry.key === null) bindings.delete(entry.command_id);
    else { prefixActionKey(entry.key); bindings.set(entry.command_id, entry.key); }
  }
  const occupied = new Set<string>();
  for (const [id, key] of bindings) {
    const normalized = `${prefixActionMode(id)}:${prefixActionKey(key)}`;
    if (occupied.has(normalized)) throw new LocalizedError("shortcuts.occupied_prefix_action", { key });
    occupied.add(normalized);
  }
  if (stroke) for (const [id, key] of direct) {
    if (matchesKeybinding(stroke, key, platform)) {
      // Existing shortcut files predate prefixes. Their direct bindings win
      // until the user explicitly configures a prefix.
      if (document.prefix === undefined) { stroke = null; break; }
      throw new LocalizedError("shortcuts.occupied_prefix", { command: id });
    }
  }
  return { stroke, bindings, label: stroke ? settings.key! : null };
}
