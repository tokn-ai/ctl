import type {
  Keybinding,
  ShortcutPlatform,
} from "./types";

interface KeyboardEventLike {
  code: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

const KEY_LABELS: Readonly<Record<string, string>> = {
  BracketLeft: "[",
  BracketRight: "]",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Space: "Space",
};

export function detectShortcutPlatform(): ShortcutPlatform {
  return /Mac|iPhone|iPad/.test(navigator.platform) ? "macos" : "other";
}

export function matchesKeybinding(
  event: KeyboardEventLike,
  keybinding: Keybinding,
  platform: ShortcutPlatform,
): boolean {
  const primaryPressed = platform === "macos" ? event.metaKey : event.ctrlKey;
  const unexpectedPlatformModifier =
    platform === "macos" ? event.ctrlKey : event.metaKey;

  return (
    event.code === keybinding.code &&
    primaryPressed === keybinding.primary &&
    !unexpectedPlatformModifier &&
    event.shiftKey === Boolean(keybinding.shift) &&
    event.altKey === Boolean(keybinding.alt)
  );
}

export function formatKeybinding(
  keybinding: Keybinding,
  platform: ShortcutPlatform,
): string {
  const key = keyLabel(keybinding.code);
  if (platform === "macos") {
    return [
      keybinding.primary ? "⌘" : "",
      keybinding.alt ? "⌥" : "",
      keybinding.shift ? "⇧" : "",
      key,
    ].join("");
  }

  return [
    keybinding.primary ? "Ctrl" : null,
    keybinding.alt ? "Alt" : null,
    keybinding.shift ? "Shift" : null,
    key,
  ]
    .filter((part): part is string => part !== null)
    .join("+");
}

function keyLabel(code: string): string {
  const known = KEY_LABELS[code];
  if (known) {
    return known;
  }
  if (code.startsWith("Key") && code.length === 4) {
    return code.slice(3);
  }
  if (code.startsWith("Digit") && code.length === 6) {
    return code.slice(5);
  }
  return code;
}
