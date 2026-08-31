export type ShortcutPlatform = "macos" | "other";

export interface Keybinding {
  code: string;
  primary: boolean;
  shift?: boolean;
  alt?: boolean;
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
  disabledReason?: string;
  visibleInPalette?: boolean;
  /** Keep the palette open when this starts a staged command flow. */
  keepPaletteOpen?: boolean;
  focusTerminalAfterRun?: boolean;
  run(): void;
}
