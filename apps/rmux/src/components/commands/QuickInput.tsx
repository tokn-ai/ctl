import { useRef, useState } from "react";
import {
  useCommandEnvironment,
  useCommandScope,
} from "../../features/commands/CommandContext";
import { QUICK_INPUT_IDS } from "../../features/commands/commandIds";
import { defaultKeybindings } from "../../features/commands/keymap";
import {
  detectShortcutPlatform,
  formatKeybinding,
} from "../../features/commands/keybindings";
import { QuickInputFrame } from "./QuickInputFrame";
import { QuickInputField, type QuickInputFieldMode } from "./QuickInputField";

export type QuickInputMode =
  | QuickInputFieldMode
  | {
      kind: "pick";
      choices: readonly { id: string; label: string; detail?: string }[];
    }
  | { kind: "confirm"; confirm_label: string; destructive?: boolean }
  | { kind: "progress"; message?: string };

interface QuickInputProps {
  title: string;
  description?: string;
  error?: string | null;
  mode: QuickInputMode;
  onSubmit(value: string): void | Promise<void>;
  onCancel(): void;
  onBack?(): void;
  onChange?(value: string): void;
  cancel_disabled?: boolean;
  /** Reuse the initiating command's configured shortcut in this confirmation. */
  confirm_command_id?: string;
}

/** Remount with a step key so secrets and drafts never leak between prompts. */
export function QuickInput({
  title,
  description,
  error,
  mode,
  onSubmit,
  onCancel,
  onBack,
  onChange,
  cancel_disabled = false,
  confirm_command_id,
}: QuickInputProps) {
  const [selected, setSelected] = useState(0);
  const inputValue = useRef<() => string>(() => "");
  const environment = useCommandEnvironment();
  const dispatcher = useCommandScope({
    commands: [
      {
        id: QUICK_INPUT_IDS.cancel,
        category: "Dialog",
        title: "Cancel Quick Input",
        enabled: !cancel_disabled,
        run: onCancel,
      },
      {
        id: QUICK_INPUT_IDS.back,
        category: "Dialog",
        title: "Previous Step",
        enabled: Boolean(onBack) && !cancel_disabled,
        run: onBack ?? (() => {}),
      },
      {
        id: QUICK_INPUT_IDS.accept,
        category: "Dialog",
        title: "Accept Quick Input",
        enabled:
          mode.kind !== "progress" &&
          (mode.kind !== "pick" || mode.choices.length > 0),
        run: (args) => {
          const value =
            args?.value ??
            (mode.kind === "input"
              ? inputValue.current()
              : mode.kind === "pick"
                ? mode.choices[selected]?.id
                : "confirm");
          if (value !== undefined) return onSubmit(value);
        },
      },
    ],
    redirects: confirm_command_id
      ? { [confirm_command_id]: QUICK_INPUT_IDS.accept }
      : undefined,
  });
  const cancel = () => {
    dispatcher.execute(QUICK_INPUT_IDS.cancel);
  };
  const submit = (value: string) => {
    dispatcher.execute(QUICK_INPUT_IDS.accept, { value });
  };
  const platform = detectShortcutPlatform();
  const cancelKey = environment
    ? environment.keybinding(QUICK_INPUT_IDS.cancel)
    : defaultKeybindings(platform).get(QUICK_INPUT_IDS.cancel);
  return (
    <QuickInputFrame
      title={title}
      onDismiss={cancel}
      onKeyDown={(event) => {
        if (mode.kind !== "pick" || !mode.choices.length) return;
        if (
          !(event.target instanceof Element) ||
          !event.target.closest('[role="listbox"]')
        )
          return;
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          const direction = event.key === "ArrowDown" ? 1 : -1;
          const next =
            (selected + direction + mode.choices.length) % mode.choices.length;
          event.currentTarget
            .querySelectorAll<HTMLElement>('[role="option"]')
            [next]?.focus();
        } else if (event.key === "Enter") {
          event.preventDefault();
          submit(mode.choices[selected].id);
        }
      }}
    >
      <header className="quick-input-heading">
        {onBack ? (
          <button
            type="button"
            aria-label="Previous step"
            onClick={() => dispatcher.execute(QUICK_INPUT_IDS.back)}
          >
            ←
          </button>
        ) : null}
        <strong>{title}</strong>
        <button
          type="button"
          onClick={cancel}
          disabled={cancel_disabled}
          aria-label="Cancel quick input"
        >
          {cancelKey ? formatKeybinding(cancelKey, platform) : "Cancel"}
        </button>
      </header>
      {description ? (
        <p className="quick-input-description">{description}</p>
      ) : null}
      {mode.kind === "input" ? (
        <QuickInputField
          mode={mode}
          onSubmit={submit}
          onChange={onChange}
          submissionValue={inputValue}
        />
      ) : null}
      {mode.kind === "pick" ? (
        <div
          className="command-palette-results"
          role="listbox"
          aria-label={title}
        >
          {mode.choices.map((choice, index) => (
            <button
              type="button"
              key={choice.id}
              role="option"
              aria-selected={index === selected}
              className={`command-palette-option ${index === selected ? "selected" : ""}`}
              autoFocus={index === 0}
              onFocus={() => setSelected(index)}
              onClick={() => submit(choice.id)}
            >
              <span>
                <strong>{choice.label}</strong>
                {choice.detail ? (
                  <small className="quick-input-detail">{choice.detail}</small>
                ) : null}
              </span>
            </button>
          ))}
        </div>
      ) : null}
      {mode.kind === "confirm" ? (
        <div className="quick-input-actions">
          <button
            type="button"
            onClick={cancel}
            disabled={cancel_disabled}
            autoFocus
          >
            Cancel
          </button>
          <button
            type="button"
            className={
              mode.destructive ? "quick-input-danger" : "button-primary"
            }
            onClick={() => submit("confirm")}
          >
            {mode.confirm_label}
          </button>
        </div>
      ) : null}
      {mode.kind === "progress" ? (
        <p className="quick-input-description" role="status">
          {mode.message ?? "Connecting…"}
        </p>
      ) : null}
      {error ? (
        <p className="quick-input-error" role="alert">
          {error}
        </p>
      ) : null}
    </QuickInputFrame>
  );
}
