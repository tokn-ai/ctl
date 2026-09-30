import { useEffect, useRef } from "react";
import { matchesKeybinding } from "./keybindings";
import type { AppCommand, Keybinding, ShortcutPlatform } from "./types";

export function isWebviewKeybinding(
  command: AppCommand,
  platform: ShortcutPlatform,
): command is AppCommand & { keybinding: Keybinding } {
  return Boolean(
    command.keybinding &&
      !(platform === "macos" && command.macosNativeKeybinding),
  );
}

export function useCommandShortcuts(
  commands: readonly AppCommand[],
  platform: ShortcutPlatform,
  onExecute: (command: AppCommand) => void,
): void {
  const commandsRef = useRef(commands);
  const executeRef = useRef(onExecute);
  commandsRef.current = commands;
  executeRef.current = onExecute;

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.repeat) {
        return;
      }
      const command = commandsRef.current.find(
        (candidate) =>
          isWebviewKeybinding(candidate, platform) &&
          matchesKeybinding(
            {
              code: event.code || event.key,
              ctrlKey: event.ctrlKey,
              metaKey: event.metaKey,
              shiftKey: event.shiftKey,
              altKey: event.altKey,
            },
            candidate.keybinding,
            platform,
          ),
      );
      if (!command) {
        return;
      }

      event.preventDefault();
      event.stopPropagation();
      if (command.enabled) {
        executeRef.current(command);
      }
    };

    window.addEventListener("keydown", handleKeyDown, true);
    return () => window.removeEventListener("keydown", handleKeyDown, true);
  }, [platform]);
}
