import type { CommandKeybinding } from "../../lib/types";
import { formatText, type Locale, type LocalizedMessage } from "../i18n/messages";

export type ShortcutPlatform = "macos" | "other";

export type Keybinding = CommandKeybinding;

/** Explicit targets come from UI selection; omission uses the active context. */
export interface CommandArguments {
  session_key?: string;
  target_key?: string;
  value?: string;
}

export interface AppCommand {
  id: string;
  category: string;
  title: string;
  category_text?: LocalizedMessage;
  title_text?: LocalizedMessage;
  detail?: string;
  keywords?: readonly string[];
  keybinding?: Keybinding;
  /** The native menu dispatches this shortcut on macOS to avoid duplicate handling. */
  macosNativeKeybinding?: boolean;
  enabled: boolean;
  /** Navigation can replace an in-flight selection; mutations stay single-flight. */
  allow_concurrent?: boolean;
  isEnabled?(args: CommandArguments): boolean;
  disabledReason?: string;
  visibleInPalette?: boolean;
  /** Keep the palette open when this starts a staged command flow. */
  keepPaletteOpen?: boolean;
  focusTerminalAfterRun?: boolean;
  run(args?: CommandArguments): void | Promise<void>;
}

export type LocalizedCommand = Omit<AppCommand, "title" | "category" | "title_text" | "category_text"> & {
  title: LocalizedMessage;
  category: LocalizedMessage;
};

export function localizeCommand(locale: Locale, { title, category, ...command }: LocalizedCommand): AppCommand {
  return {
    ...command,
    title: formatText(locale, title),
    category: formatText(locale, category),
    title_text: title,
    category_text: category,
  };
}
