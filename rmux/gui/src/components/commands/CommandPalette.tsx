import { useMemo, useState } from "react";
import {
  firstEnabledIndex,
  nextEnabledIndex,
  searchCommands,
} from "../../features/commands/commandSearch";
import { formatKeybinding } from "../../features/commands/keybindings";
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
      onExecute(command);
    }
  }

  return (
    <div
      className="command-palette-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onDismiss();
        }
      }}
    >
      <section
        className="command-palette"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing) {
            return;
          }
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
              execute(selectedCommand);
              break;
            case "Escape":
              event.preventDefault();
              onDismiss();
              break;
            case "Tab":
              event.preventDefault();
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
          <kbd>Esc</kbd>
        </div>

        <div
          id="command-palette-results"
          className="command-palette-results"
          role="listbox"
        >
          {filteredCommands.length === 0 ? (
            <div className="command-palette-empty">No matching commands.</div>
          ) : null}
          {filteredCommands.map((command, index) => {
            const selected = index === selectedIndex;
            return (
              <button
                id={optionId(command.id)}
                className={`command-palette-option ${
                  selected ? "selected" : ""
                }`}
                type="button"
                role="option"
                aria-selected={selected}
                aria-disabled={!command.enabled}
                tabIndex={-1}
                key={command.id}
                onMouseEnter={() => {
                  if (command.enabled) {
                    setSelectedCommandId(command.id);
                  }
                }}
                onClick={() => execute(command)}
              >
                <span className="command-palette-option-copy">
                  <span>
                    <small>{command.category}</small>
                    <strong>{command.title}</strong>
                  </span>
                  {command.detail || (!command.enabled && command.disabledReason) ? (
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
      </section>
    </div>
  );
}

function optionId(commandId: string): string {
  return `command-option-${commandId.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}
