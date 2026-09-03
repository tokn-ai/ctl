import { useId, useRef, useState, type RefObject } from "react";

export interface QuickInputSuggestions {
  label: string;
  items: readonly { id: string; label: string }[];
  loading?: boolean;
  loading_message?: string;
  empty_message?: string;
  no_match_message?: string;
  warning?: string;
}

export interface QuickInputFieldMode {
  kind: "input";
  label: string;
  initial_value?: string;
  placeholder?: string;
  secret?: boolean;
  submit_label?: string;
  suggestions?: QuickInputSuggestions;
}

interface QuickInputFieldProps {
  mode: QuickInputFieldMode;
  onSubmit(value: string): void;
  onChange?(value: string): void;
  submissionValue?: RefObject<() => string>;
}

/** Editable input with optional suggestions; selection never overwrites a draft. */
export function QuickInputField({
  mode,
  onSubmit,
  onChange,
  submissionValue,
}: QuickInputFieldProps) {
  const [value, setValue] = useState(mode.initial_value ?? "");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const listId = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const suggestions = mode.secret ? undefined : mode.suggestions;
  const query = value.trim().toLowerCase();
  const items =
    suggestions?.items.filter((item) =>
      [item.label, item.id].some((text) => text.toLowerCase().includes(query)),
    ) ?? [];
  const selectedIndex = items.findIndex((item) => item.id === selectedId);
  const selected = items[selectedIndex];
  if (submissionValue) submissionValue.current = () => selected?.id ?? value;
  const status = suggestions?.loading
    ? (suggestions.loading_message ?? "Loading suggestions…")
    : items.length === 0 && suggestions
      ? query
        ? (suggestions.no_match_message ??
          "No matching suggestions. You can still enter a value manually.")
        : (suggestions.empty_message ?? "No suggestions available.")
      : undefined;

  return (
    <>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit(selected?.id ?? value);
        }}
      >
        <div className="command-palette-input-row">
          <span aria-hidden="true">›</span>
          <input
            aria-label={mode.label}
            type={mode.secret ? "password" : "text"}
            role={suggestions ? "combobox" : undefined}
            aria-autocomplete={suggestions ? "list" : undefined}
            aria-controls={suggestions ? listId : undefined}
            aria-expanded={suggestions ? true : undefined}
            aria-activedescendant={
              selected ? `${listId}-${selectedIndex}` : undefined
            }
            value={value}
            onChange={(event) => {
              setValue(event.currentTarget.value);
              setSelectedId(null);
              onChange?.(event.currentTarget.value);
            }}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing) return;
              if (event.key === "Enter" && event.repeat) {
                event.preventDefault();
                return;
              }
              if (
                !items.length ||
                (event.key !== "ArrowDown" && event.key !== "ArrowUp")
              )
                return;
              event.preventDefault();
              const next =
                selectedIndex < 0
                  ? event.key === "ArrowDown"
                    ? 0
                    : items.length - 1
                  : (selectedIndex +
                      (event.key === "ArrowDown" ? 1 : -1) +
                      items.length) %
                    items.length;
              setSelectedId(items[next].id);
              listRef.current?.children[next]?.scrollIntoView?.({
                block: "nearest",
              });
            }}
            placeholder={mode.placeholder}
            autoFocus
            autoComplete="off"
            spellCheck={false}
          />
          <button type="submit">{mode.submit_label ?? "Continue"}</button>
        </div>
      </form>
      {suggestions ? (
        <>
          <div
            ref={listRef}
            id={listId}
            className="command-palette-results"
            role="listbox"
            aria-label={suggestions.label}
            aria-busy={suggestions.loading ?? false}
          >
            {items.map((item, index) => (
              <button
                type="button"
                role="option"
                tabIndex={-1}
                id={`${listId}-${index}`}
                key={item.id}
                aria-selected={item.id === selected?.id}
                className={`command-palette-option ${item.id === selected?.id ? "selected" : ""}`}
                onClick={() => onSubmit(item.id)}
              >
                {item.label}
              </button>
            ))}
          </div>
          {status ? (
            <p className="quick-input-description" role="status">
              {status}
            </p>
          ) : null}
          {suggestions.warning ? (
            <p className="quick-input-description" role="status">
              {suggestions.warning}
            </p>
          ) : null}
        </>
      ) : null}
    </>
  );
}
