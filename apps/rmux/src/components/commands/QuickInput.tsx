import { useState } from "react";
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
  onSubmit(value: string): void;
  onCancel(): void;
  onBack?(): void;
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
}: QuickInputProps) {
  const [selected, setSelected] = useState(0);
  return (
    <QuickInputFrame
      title={title}
      onDismiss={onCancel}
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
          onSubmit(mode.choices[selected].id);
        }
      }}
    >
      <header className="quick-input-heading">
        {onBack ? (
          <button type="button" aria-label="Previous step" onClick={onBack}>
            ←
          </button>
        ) : null}
        <strong>{title}</strong>
        <button
          type="button"
          onClick={onCancel}
          aria-label="Cancel quick input"
        >
          Esc
        </button>
      </header>
      {description ? (
        <p className="quick-input-description">{description}</p>
      ) : null}
      {mode.kind === "input" ? (
        <QuickInputField mode={mode} onSubmit={onSubmit} />
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
              onClick={() => onSubmit(choice.id)}
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
          <button type="button" onClick={onCancel} autoFocus>
            Cancel
          </button>
          <button
            type="button"
            className={
              mode.destructive ? "quick-input-danger" : "button-primary"
            }
            onClick={() => onSubmit("confirm")}
          >
            {mode.confirm_label}
          </button>
        </div>
      ) : null}
      {mode.kind === "progress" ? (
        <p className="quick-input-description" role="status">
          {mode.message ?? "Connecting… Press Escape to cancel."}
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
