import {
  createContext,
  useContext,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { CommandDispatcher, type CommandScope } from "./CommandDispatcher";
import type { AppCommand, Keybinding } from "./types";
import { useCommandShortcuts } from "./useCommandShortcuts";
import { detectShortcutPlatform } from "./keybindings";
import { defaultKeybindings } from "./keymap";

export interface CommandEnvironment {
  dispatcher: CommandDispatcher;
  keybinding(command_id: string): Keybinding | undefined;
}

const Context = createContext<CommandEnvironment | null>(null);

export function CommandProvider({
  value,
  children,
}: {
  value: CommandEnvironment;
  children: ReactNode;
}) {
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useCommandEnvironment() {
  return useContext(Context);
}

export function useCommandScope(scope: CommandScope) {
  const environment = useCommandEnvironment();
  // Standalone dialogs (and component tests) retain the same dispatch policy.
  const [fallback] = useState(() => new CommandDispatcher());
  const dispatcher = environment?.dispatcher ?? fallback;
  const [token] = useState(() => Symbol("command_scope"));
  const latest = useRef(scope);
  latest.current = scope;
  useLayoutEffect(() => {
    dispatcher.setScope(token, latest.current);
    return () => dispatcher.removeScope(token);
  }, [dispatcher, token]);
  useLayoutEffect(() => {
    dispatcher.setScope(token, scope);
  }, [dispatcher, token, scope]);
  const platform = detectShortcutPlatform();
  const defaults = defaultKeybindings(platform);
  useCommandShortcuts(
    environment
      ? []
      : scope.commands.map((command) => ({
          ...command,
          keybinding: defaults.get(command.id),
        })),
    platform,
    (command) => dispatcher.execute(command.id),
  );
  return dispatcher;
}

export function useEffectiveCommands(
  dispatcher: CommandDispatcher,
): AppCommand[] {
  useSyncExternalStore(dispatcher.subscribe, dispatcher.snapshot);
  return dispatcher.effectiveCommands();
}
