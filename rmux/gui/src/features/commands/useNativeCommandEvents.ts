import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";
import type { AppCommand } from "./types";

export const NATIVE_COMMAND_EVENT = "rmux://command";

export function findEnabledNativeCommand(
  commands: readonly AppCommand[],
  commandId: string,
): AppCommand | null {
  return (
    commands.find(
      (candidate) => candidate.id === commandId && candidate.enabled,
    ) ?? null
  );
}

export function useNativeCommandEvents(
  commands: readonly AppCommand[],
  onExecute: (command: AppCommand) => void,
): void {
  const commandsRef = useRef(commands);
  const executeRef = useRef(onExecute);
  commandsRef.current = commands;
  executeRef.current = onExecute;

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | null = null;

    void listen<string>(NATIVE_COMMAND_EVENT, (event) => {
      const command = findEnabledNativeCommand(
        commandsRef.current,
        event.payload,
      );
      if (command) {
        executeRef.current(command);
      }
    })
      .then((stopListening) => {
        if (disposed) {
          stopListening();
        } else {
          unlisten = stopListening;
        }
      })
      .catch((error: unknown) => {
        if (!disposed) {
          console.error("Could not register native command events", error);
        }
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}
