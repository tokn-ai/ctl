import { useEffect, useRef, useState } from "react";
import { syncCommandMenu } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import { CONFIGURABLE_COMMAND_IDS } from "./commandIds";
import { useCommandEnvironment, useEffectiveCommands } from "./CommandContext";
import { useCommandShortcuts } from "./useCommandShortcuts";
import { useNativeCommandEvents } from "./useNativeCommandEvents";
import type { ShortcutPlatform } from "./types";
import { formatKeybinding } from "./keybindings";

/** Keyboard/menu adapters consume the same resolved commands and keymap. */
export function CommandBindings({ platform }: { platform: ShortcutPlatform }) {
  const environment = useCommandEnvironment()!;
  const commands = useEffectiveCommands(environment.dispatcher).map(
    (command) => ({
      ...command,
      keybinding: environment.keybinding(command.id),
      // Command-modified keys need AppKit ownership. Plain dialog keys (Escape,
      // Enter) and Alt/function keys stay in the webview, including in Tauri.
      macosNativeKeybinding:
        platform === "macos" &&
        environment.keybinding(command.id)?.primary === true &&
        CONFIGURABLE_COMMAND_IDS.includes(command.id) &&
        "__TAURI_INTERNALS__" in window,
    }),
  );
  const [error, setError] = useState<string | null>(null);
  const payload = JSON.stringify(
    commands
      .filter((command) => CONFIGURABLE_COMMAND_IDS.includes(command.id))
      .map((command) => ({
        command_id: command.id,
        title:
          command.title +
          (command.keybinding && !command.macosNativeKeybinding
            ? ` (${formatKeybinding(command.keybinding, platform)})`
            : ""),
        keybinding:
          command.enabled && command.macosNativeKeybinding
            ? (command.keybinding ?? null)
            : null,
        enabled: command.enabled,
      })),
  );
  const queue = useRef(Promise.resolve());
  useEffect(() => {
    let disposed = false;
    queue.current = queue.current
      .catch(() => {})
      .then(async () => {
        if (disposed) return;
        try {
          await syncCommandMenu(JSON.parse(payload));
          if (!disposed) setError(null);
        } catch (failure) {
          if (!disposed)
            setError(
              `Could not update native shortcuts: ${errorMessage(failure)}`,
            );
        }
      });
    return () => {
      disposed = true;
    };
  }, [payload]);
  const execute = (command: { id: string }) => {
    environment.dispatcher.execute(command.id);
  };
  useCommandShortcuts(commands, platform, execute);
  useNativeCommandEvents(commands, execute);
  return error ? (
    <div className="message-banner" role="alert">
      {error}
    </div>
  ) : null;
}
