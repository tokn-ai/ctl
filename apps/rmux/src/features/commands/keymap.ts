import type { KeybindingsDocument } from "../../lib/types";
import {
  COMMAND_IDS,
  CONFIGURABLE_COMMAND_IDS,
  QUICK_INPUT_IDS,
} from "./commandIds";
import type { Keybinding, ShortcutPlatform } from "./types";
import { formatKeybinding } from "./keybindings";

export const EMPTY_KEYBINDINGS: KeybindingsDocument = {
  schema_version: 1,
  overrides: [],
};

export function defaultKeybindings(
  platform: ShortcutPlatform,
): ReadonlyMap<string, Keybinding> {
  const nativeShift = platform === "other";
  return new Map([
    [COMMAND_IDS.showPalette, { code: "KeyP", primary: true, shift: true }],
    [COMMAND_IDS.newShell, { code: "KeyN", primary: true, shift: true }],
    [COMMAND_IDS.newTab, { code: "KeyT", primary: true, shift: nativeShift }],
    [
      COMMAND_IDS.disconnect,
      { code: "KeyW", primary: true, shift: nativeShift },
    ],
    [COMMAND_IDS.close, { code: "KeyE", primary: true, shift: nativeShift }],
    [COMMAND_IDS.nextTab, { code: "BracketRight", primary: true, shift: true }],
    [
      COMMAND_IDS.previousTab,
      { code: "BracketLeft", primary: true, shift: true },
    ],
    [QUICK_INPUT_IDS.cancel, { code: "Escape", primary: false }],
  ]);
}

const SPECIAL_KEYS: Readonly<Record<string, string>> = {
  escape: "Escape",
  esc: "Escape",
  enter: "Enter",
  space: "Space",
  backspace: "Backspace",
  delete: "Delete",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
  up: "ArrowUp",
  down: "ArrowDown",
  left: "ArrowLeft",
  right: "ArrowRight",
  "[": "BracketLeft",
  "]": "BracketRight",
  ",": "Comma",
  ".": "Period",
  "/": "Slash",
  "\\": "Backslash",
  "-": "Minus",
  "=": "Equal",
};
const VALID_CODE =
  /^(Key[A-Z]|Digit[0-9]|F([1-9]|1[0-2])|Escape|Enter|Space|Backspace|Delete|Home|End|PageUp|PageDown|Arrow(Up|Down|Left|Right)|Bracket(Left|Right)|Comma|Period|Slash|Backslash|Minus|Equal)$/;

export function parseKeybinding(text: string): Keybinding | null {
  if (!text.trim()) return null;
  const parts = text
    .trim()
    .split("+")
    .map((part) => part.trim());
  const key = parts.pop()!;
  const modifiers = parts.map((part) => part.toLowerCase());
  const primary = modifiers.filter((part) =>
    ["primary", "cmdorctrl", "cmd", "ctrl", "command", "control"].includes(
      part,
    ),
  );
  if (
    primary.length > 1 ||
    new Set(modifiers).size !== modifiers.length ||
    modifiers.some(
      (part) =>
        ![
          "primary",
          "cmdorctrl",
          "cmd",
          "ctrl",
          "command",
          "control",
          "shift",
          "alt",
          "option",
        ].includes(part),
    ) ||
    (modifiers.includes("alt") && modifiers.includes("option"))
  ) {
    throw new Error(
      "Use a shortcut such as Primary+Shift+E, Alt+F2, or Escape.",
    );
  }
  const code = /^[a-z]$/i.test(key)
    ? `Key${key.toUpperCase()}`
    : /^\d$/.test(key)
      ? `Digit${key}`
      : VALID_CODE.test(key)
        ? key
        : (SPECIAL_KEYS[key.toLowerCase()] ?? key.toUpperCase());
  if (!VALID_CODE.test(code))
    throw new Error(`Unsupported shortcut key: ${key}`);
  return {
    code,
    primary: primary.length === 1,
    shift: modifiers.includes("shift"),
    alt: modifiers.includes("alt") || modifiers.includes("option"),
  };
}

export function editableKeybinding(binding: Keybinding | undefined): string {
  if (!binding) return "";
  return formatKeybinding(binding, "other").replace(/^Ctrl/, "Primary");
}

export function resolveKeymap(
  document: KeybindingsDocument,
  platform: ShortcutPlatform,
): ReadonlyMap<string, Keybinding> {
  if (
    document.schema_version !== 1 ||
    !Array.isArray(document.overrides) ||
    document.overrides.length > CONFIGURABLE_COMMAND_IDS.length
  )
    throw new Error("Unsupported keyboard shortcut settings.");
  const bindings = new Map(defaultKeybindings(platform));
  const seen = new Set<string>();
  for (const override of document.overrides) {
    if (
      !CONFIGURABLE_COMMAND_IDS.includes(override.command_id) ||
      seen.has(override.command_id)
    )
      throw new Error(`Unknown or duplicate command: ${override.command_id}`);
    seen.add(override.command_id);
    const key = override.keybinding;
    if (
      key !== null &&
      (!VALID_CODE.test(key.code) ||
        typeof key.primary !== "boolean" ||
        (key.shift !== undefined && typeof key.shift !== "boolean") ||
        (key.alt !== undefined && typeof key.alt !== "boolean"))
    )
      throw new Error(`Invalid shortcut for ${override.command_id}`);
    if (key) bindings.set(override.command_id, key);
    else bindings.delete(override.command_id);
  }
  const occupied = new Map<string, string>();
  for (const [id, binding] of bindings) {
    const scoped = id.startsWith("quick_input.");
    if (
      !scoped &&
      !binding.primary &&
      !binding.alt &&
      !/^F\d+$/.test(binding.code)
    )
      throw new Error(
        `${id} needs Primary, Alt, or a function key to avoid capturing terminal typing.`,
      );
    // Native edit/window equivalents remain owned by the platform.
    if (
      platform === "macos" &&
      binding.primary &&
      ((!binding.alt &&
        !binding.shift &&
        [
          "KeyQ",
          "KeyH",
          "KeyM",
          "KeyC",
          "KeyV",
          "KeyX",
          "KeyA",
          "KeyZ",
        ].includes(binding.code)) ||
        (!binding.alt && binding.shift && binding.code === "KeyZ") ||
        (binding.alt && !binding.shift && binding.code === "KeyH"))
    )
      throw new Error(
        `${formatKeybinding(binding, platform)} is reserved for native editing or window commands.`,
      );
    // Keep native menu equivalents unambiguous, including dialog redirects.
    const signature = formatKeybinding(binding, "other");
    const other = occupied.get(signature);
    if (other)
      throw new Error(
        `${formatKeybinding(binding, platform)} is already assigned to ${other}. Unbind that command first.`,
      );
    occupied.set(signature, id);
  }
  return bindings;
}
