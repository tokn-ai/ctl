import type { CommandKeybinding } from "../../lib/types";

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
  detail?: string;
  keywords?: readonly string[];
  keybinding?: Keybinding;
  /** The native menu dispatches this shortcut on macOS to avoid duplicate handling. */
  macosNativeKeybinding?: boolean;
  enabled: boolean;
  isEnabled?(args: CommandArguments): boolean;
  disabledReason?: string;
  visibleInPalette?: boolean;
  /** Keep the palette open when this starts a staged command flow. */
  keepPaletteOpen?: boolean;
  focusTerminalAfterRun?: boolean;
  run(args?: CommandArguments): void | Promise<void>;
}
