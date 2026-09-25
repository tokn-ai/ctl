import { useMemo, useRef, useState } from "react";
import {
  firstEnabledIndex,
  nextEnabledIndex,
  searchCommands,
} from "../../features/commands/commandSearch";
import { formatKeybinding } from "../../features/commands/keybindings";
import { PointerSelectionIntent } from "../../features/commands/PointerSelectionIntent";
import { QuickInputFrame } from "./QuickInputFrame";
import {
  useCommandEnvironment,
  useCommandScope,
} from "../../features/commands/CommandContext";
import { defaultKeybindings } from "../../features/commands/keymap";
import { QUICK_INPUT_IDS } from "../../features/commands/commandIds";
import type {
  AppCommand,
  ShortcutPlatform,
} from "../../features/commands/types";

interface CommandPaletteProps {
  commands: readonly AppCommand[];
  platform: ShortcutPlatform;
  onDismiss(): void;
  onExecute(command: AppCommand): void;
}

export function CommandPalette({
  commands,
  platform,
  onDismiss,
  onExecute,
}: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [selectedCommandId, setSelectedCommandId] = useState<string | null>(
    null,
  );
  const pointerSelectionRef = useRef(new PointerSelectionIntent());
  const environment = useCommandEnvironment();
  const cancelKey = environment
    ? environment.keybinding(QUICK_INPUT_IDS.cancel)
    : defaultKeybindings(platform).get(QUICK_INPUT_IDS.cancel);
  const filteredCommands = useMemo(
    () => searchCommands(commands, query),
    [commands, query],
  );
  const explicitIndex = filteredCommands.findIndex(
    (command) => command.id === selectedCommandId && command.enabled,
  );
  const selectedIndex =
    explicitIndex === -1 ? firstEnabledIndex(filteredCommands) : explicitIndex;
  const selectedCommand =
    selectedIndex === -1 ? null : filteredCommands[selectedIndex];

  function moveSelection(direction: 1 | -1) {
    const nextIndex = nextEnabledIndex(
      filteredCommands,
      selectedIndex,
      direction,
    );
    if (nextIndex !== -1) {
      const next = filteredCommands[nextIndex];
      setSelectedCommandId(next.id);
      document
        .getElementById(optionId(next.id))
        ?.scrollIntoView({ block: "nearest" });
    }
  }

  function execute(command: AppCommand | null) {
    if (command?.enabled) {
      setSelectedCommandId(command.id);
      onExecute(command);
    }
  }

  const dispatcher = useCommandScope({
    allow_app_commands: true,
    commands: [
      {
        id: QUICK_INPUT_IDS.cancel,
        category: "Dialog",
        title: "Close Command Palette",
        enabled: true,
        run: onDismiss,
      },
      {
        id: QUICK_INPUT_IDS.accept,
        category: "Dialog",
        title: "Run Selected Command",
        enabled: selectedCommand !== null,
        run: () => execute(selectedCommand),
      },
    ],
  });

  return (
    <QuickInputFrame
      title="Command palette"
      onDismiss={() => {
        dispatcher.execute(QUICK_INPUT_IDS.cancel);
      }}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing) {
          return;
        }
        // Enter on the close button must activate that button, not the result.
        if (
          event.target instanceof HTMLButtonElement &&
          event.target.getAttribute("role") !== "option"
        )
          return;
        switch (event.key) {
          case "ArrowDown":
            event.preventDefault();
            moveSelection(1);
            break;
          case "ArrowUp":
            event.preventDefault();
            moveSelection(-1);
            break;
          case "Enter":
            event.preventDefault();
            dispatcher.execute(QUICK_INPUT_IDS.accept);
            break;
        }
      }}
    >
      <div className="command-palette-input-row">
        <span aria-hidden="true">›</span>
        <input
          value={query}
          onChange={(event) => {
            setQuery(event.currentTarget.value);
            setSelectedCommandId(null);
          }}
          role="combobox"
          aria-label="Search commands"
          aria-autocomplete="list"
          aria-controls="command-palette-results"
          aria-expanded="true"
          aria-activedescendant={
            selectedCommand ? optionId(selectedCommand.id) : undefined
          }
          placeholder="Type a command"
          autoComplete="off"
          spellCheck={false}
          autoFocus
        />
        <button
          type="button"
          aria-label="Close command palette"
          onClick={() => dispatcher.execute(QUICK_INPUT_IDS.cancel)}
        >
          {cancelKey ? formatKeybinding(cancelKey, platform) : "Close"}
        </button>
      </div>

      <div
        id="command-palette-results"
        className="command-palette-results"
        role="listbox"
        onPointerMove={(event) => {
          if (event.pointerType === "touch") {
            return;
          }
          const commandId = commandIdFromElement(event.target);
          const selectedId = pointerSelectionRef.current.move(
            { clientX: event.clientX, clientY: event.clientY },
            commandId,
          );
          if (
            selectedId &&
            filteredCommands.some(
              (command) => command.id === selectedId && command.enabled,
            )
          ) {
            setSelectedCommandId(selectedId);
          }
        }}
        onPointerLeave={() => pointerSelectionRef.current.leave()}
        onScroll={() => {
          const position = pointerSelectionRef.current.currentPosition();
          if (!position) {
            return;
          }
          pointerSelectionRef.current.scrolled(
            commandIdAtPoint(position.clientX, position.clientY),
          );
        }}
      >
        {filteredCommands.length === 0 ? (
          <div className="command-palette-empty">No matching commands.</div>
        ) : null}
        {filteredCommands.map((command, index) => {
          const selected = index === selectedIndex;
          return (
            <button
              id={optionId(command.id)}
              className={`command-palette-option ${selected ? "selected" : ""}`}
              type="button"
              role="option"
              aria-selected={selected}
              aria-disabled={!command.enabled}
              tabIndex={-1}
              key={command.id}
              data-command-id={command.id}
              onClick={() => execute(command)}
            >
              <span className="command-palette-option-copy">
                <span>
                  <small>{command.category}</small>
                  <strong>{command.title}</strong>
                </span>
                {command.detail ||
                (!command.enabled && command.disabledReason) ? (
                  <em>
                    {command.enabled ? command.detail : command.disabledReason}
                  </em>
                ) : null}
              </span>
              {command.keybinding ? (
                <kbd>{formatKeybinding(command.keybinding, platform)}</kbd>
              ) : null}
            </button>
          );
        })}
      </div>
    </QuickInputFrame>
  );
}

function optionId(commandId: string): string {
  return `command-option-${commandId.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}

function commandIdAtPoint(clientX: number, clientY: number): string | null {
  return commandIdFromElement(document.elementFromPoint(clientX, clientY));
}

function commandIdFromElement(target: EventTarget | null): string | null {
  if (!(target instanceof Element)) {
    return null;
  }
  return (
    target.closest<HTMLElement>("[data-command-id]")?.dataset.commandId ?? null
  );
}
