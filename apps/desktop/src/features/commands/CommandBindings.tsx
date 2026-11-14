import { useEffect, useRef, useState } from "react";
import { syncCommandMenu, type NativeMenuLabels } from "../../lib/tauri";
import { localizedErrorText } from "../../lib/errors";
import { CONFIGURABLE_COMMAND_IDS } from "./commandIds";
import { useCommandEnvironment, useEffectiveCommands } from "./CommandContext";
import { useCommandShortcuts } from "./useCommandShortcuts";
import { useNativeCommandEvents } from "./useNativeCommandEvents";
import type { AppCommand, ShortcutPlatform } from "./types";
import { formatKeybinding } from "./keybindings";
import { useNotificationSource } from "../notifications/NotificationContext";
import { useLocale } from "../i18n/LocaleContext";
import { message, type DisplayText, type Locale, translate } from "../i18n/messages";
import type { NativeCommandBinding, NativeCommandMenuResult, NotificationInput } from "../../lib/types";

/** Keyboard/menu adapters consume the same resolved commands and keymap. */
export function CommandBindings({ platform }: { platform: ShortcutPlatform }) {
  const { locale } = useLocale();
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
  const current_commands = useRef(commands);
  current_commands.current = commands;
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
          const result = await syncCommandMenu(bindings, nativeMenuLabels(locale));
          if (!disposed) setNotification(result.rejected.length ? {
            severity: "warning",
            title: message("native_menu.commands_unavailable"),
            message: message("native_menu.rejected_commands", {
              detail: rejectedCommandDetails(result, bindings, current_commands.current),
              count: Math.max(0, result.rejected.length - 3),
            }),
            source: message("native_menu.keyboard_shortcuts"),
          } : null);
        } catch (failure) {
          if (!disposed)
            setNotification({
              severity: "error",
              title: message("native_menu.native_shortcuts"),
              message: message("native_menu.sync_failed", { detail: localizedErrorText(failure) }),
              source: message("native_menu.keyboard_shortcuts"),
            });
        }
      });
    return () => {
      disposed = true;
    };
  }, [locale, payload]);
  const execute = (command: { id: string }) => {
    environment.dispatcher.execute(command.id);
  };
  useCommandShortcuts(commands, platform, execute);
  useNativeCommandEvents(commands, execute);
  return null;
}

function rejectedCommandDetails(result: NativeCommandMenuResult, bindings: readonly NativeCommandBinding[], commands: readonly AppCommand[]): DisplayText {
  // The formatted native title can change while this remains the same incident.
  const [first, second, third] = result.rejected.slice(0, 3).map((rejection) => {
    const id = bindings[rejection.index]?.command_id ?? rejection.command_id;
    const command = commands.find((entry) => entry.id === id)?.title_text ?? id
      ?? message("native_menu.command_number", { index: rejection.index + 1 });
    return message("native_menu.diagnostic", { command, detail: rejection.message });
  });
  const next = (detail: DisplayText | undefined) => detail ? message("native_menu.diagnostic_next", { detail }) : "";
  return message("native_menu.diagnostic_rows", { first: first ?? "", second: next(second), third: next(third) });
}

function nativeMenuLabels(locale: Locale): NativeMenuLabels {
  return {
    about: translate(locale, "about.title"),
    commands: translate(locale, "native_menu.commands"),
    edit: translate(locale, "native_menu.edit"),
    view: translate(locale, "native_menu.view"),
    window: translate(locale, "native_menu.window"),
    help: translate(locale, "native_menu.help"),
    services: translate(locale, "native_menu.services"),
    hide: translate(locale, "native_menu.hide"),
    hide_others: translate(locale, "native_menu.hide_others"),
    quit: translate(locale, "native_menu.quit"),
    undo: translate(locale, "native_menu.undo"),
    redo: translate(locale, "native_menu.redo"),
    cut: translate(locale, "native_menu.cut"),
    copy: translate(locale, "native_menu.copy"),
    paste: translate(locale, "native_menu.paste"),
    select_all: translate(locale, "native_menu.select_all"),
    fullscreen: translate(locale, "native_menu.fullscreen"),
    minimize: translate(locale, "native_menu.minimize"),
    maximize: translate(locale, "native_menu.maximize"),
  };
}
