import { useEffect, useRef, useState } from "react";
import { syncCommandMenu } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import { CONFIGURABLE_COMMAND_IDS } from "./commandIds";
import { useCommandEnvironment, useEffectiveCommands } from "./CommandContext";
import { useCommandShortcuts } from "./useCommandShortcuts";
import { useNativeCommandEvents } from "./useNativeCommandEvents";
import type { ShortcutPlatform } from "./types";
import { formatKeybinding } from "./keybindings";
import { useNotificationSource } from "../notifications/NotificationContext";
import type { NativeCommandBinding, NativeCommandMenuResult, NotificationInput } from "../../lib/types";

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
  const [notification, setNotification] = useState<NotificationInput | null>(null);
  useNotificationSource("native_shortcuts", notification);
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
          const bindings: NativeCommandBinding[] = JSON.parse(payload);
          const result = await syncCommandMenu(bindings);
          if (!disposed) setNotification(result.rejected.length ? {
            severity: "warning",
            title: "Some menu commands are unavailable",
            message: rejectedCommandMessage(result, bindings),
            source: "Keyboard shortcuts",
          } : null);
        } catch (failure) {
          if (!disposed)
            setNotification({
              severity: "error",
              title: "Native shortcuts",
              message: `Could not update native shortcuts: ${errorMessage(failure)}`,
              source: "Keyboard shortcuts",
            });
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
  return null;
}

function rejectedCommandMessage(result: NativeCommandMenuResult, bindings: readonly NativeCommandBinding[]): string {
  const messages = result.rejected.slice(0, 3).map((rejection) => {
    const title = bindings[rejection.index]?.title || rejection.command_id || `Command ${rejection.index + 1}`;
    return `${title}: ${rejection.message}`;
  });
  if (result.rejected.length > messages.length) messages.push(`${result.rejected.length - messages.length} more commands unavailable.`);
  return messages.join("\n");
}
