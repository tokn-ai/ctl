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
  enabled: boolean;
  disabledReason?: string;
  visibleInPalette?: boolean;
  focusTerminalAfterRun?: boolean;
  run(): void;
}
