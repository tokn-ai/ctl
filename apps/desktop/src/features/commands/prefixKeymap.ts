import type { KeybindingsDocument, TerminalPrefixSettings } from "../../lib/types";
import type { Keybinding, ShortcutPlatform } from "./types";
import { matchesKeybinding } from "./keybindings";

export const PREFIX_ACTIONS = [
  { id: "pane.split_right", title: "Split pane right", key: "V" },
  { id: "pane.split_below", title: "Split pane below", key: "S" },
  { id: "pane.focus_left", title: "Focus left", key: "Left" },
  { id: "pane.focus_right", title: "Focus right", key: "Right" },
  { id: "pane.focus_up", title: "Focus above", key: "Up" },
  { id: "pane.focus_down", title: "Focus below", key: "Down" },
  { id: "pane.resize_left", title: "Resize pane left", key: "Ctrl+Left" },
  { id: "pane.resize_right", title: "Resize pane right", key: "Ctrl+Right" },
  { id: "pane.resize_up", title: "Resize pane up", key: "Ctrl+Up" },
  { id: "pane.resize_down", title: "Resize pane down", key: "Ctrl+Down" },
  { id: "pane.resize_left_large", title: "Resize pane left by 5 cells", key: "Alt+Left" },
  { id: "pane.resize_right_large", title: "Resize pane right by 5 cells", key: "Alt+Right" },
  { id: "pane.resize_up_large", title: "Resize pane up by 5 cells", key: "Alt+Up" },
  { id: "pane.resize_down_large", title: "Resize pane down by 5 cells", key: "Alt+Down" },
  { id: "pane.zoom", title: "Zoom pane", key: "Z" },
  { id: "pane.move", title: "Move pane mode", key: "M" },
  { id: "pane.move_left", title: "Move pane left", key: "Left" },
  { id: "pane.move_right", title: "Move pane right", key: "Right" },
  { id: "pane.move_up", title: "Move pane up", key: "Up" },
  { id: "pane.move_down", title: "Move pane down", key: "Down" },
  { id: "pane.promote", title: "Move pane to new session", key: "!" },
  { id: "tab.next", title: "Next session tab", key: "N" },
  { id: "tab.previous", title: "Previous session tab", key: "P" },
  { id: "tab.new_shell_here", title: "New shell here", key: "C" },
  { id: "view.show_command_palette", title: "Command palette", key: ":" },
] as const;

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
  if (!match) throw new Error("Use Ctrl+letter or Alt+letter for the terminal prefix, such as Ctrl+B.");
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
  throw new Error("Use a letter, digit, arrow name (optionally Ctrl+ or Alt+), !, :, %, or a double quote.");
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
    throw new Error("Invalid terminal prefix bindings.");
  const bindings = new Map<string, string>(PREFIX_ACTIONS.map((action) => [action.id, action.key]));
  const seen = new Set<string>();
  for (const entry of settings.bindings) {
    if (!bindings.has(entry.command_id) || seen.has(entry.command_id)) throw new Error("Unknown or duplicate prefix action.");
    seen.add(entry.command_id);
    if (entry.key === null) bindings.delete(entry.command_id);
    else { prefixActionKey(entry.key); bindings.set(entry.command_id, entry.key); }
  }
  const occupied = new Set<string>();
  for (const [id, key] of bindings) {
    const normalized = `${prefixActionMode(id)}:${prefixActionKey(key)}`;
    if (occupied.has(normalized)) throw new Error(`${key} is already assigned to another prefix action.`);
    occupied.add(normalized);
  }
  if (stroke) for (const [id, key] of direct) {
    if (matchesKeybinding(stroke, key, platform)) {
      // Existing shortcut files predate prefixes. Their direct bindings win
      // until the user explicitly configures a prefix.
      if (document.prefix === undefined) { stroke = null; break; }
      throw new Error(`The prefix is already assigned to ${id}. Choose another prefix or unbind that shortcut.`);
    }
  }
  return { stroke, bindings, label: stroke ? settings.key! : "Disabled" };
}
