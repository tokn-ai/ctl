import type { FormEvent } from "react";
import { useState } from "react";

interface SshHostPickerProps {
  suggestions: readonly string[];
  warning: string | null;
  onAddHost(destination: string): boolean;
  onClose(): void;
}

export function SshHostPicker({
  suggestions,
  warning,
  onAddHost,
  onClose,
}: SshHostPickerProps) {
  const [destination, setDestination] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);

  function addHost(candidate: string) {
    if (!onAddHost(candidate)) {
      setValidationError("Enter a unique OpenSSH host or alias.");
      return;
    }
    onClose();
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    addHost(destination);
  }

  return (
    <form className="host-form" onSubmit={submit}>
      {suggestions.length > 0 ? (
        <div className="host-suggestions">
          <span>From SSH config</span>
          <div>
            {suggestions.map((candidate) => (
              <button
                type="button"
                key={candidate}
                onClick={() => addHost(candidate)}
                aria-label={`Add ${candidate}`}
                title={`Add ${candidate}`}
              >
                {candidate}
              </button>
            ))}
          </div>
        </div>
      ) : null}
      <label>
        Other OpenSSH host or alias
        <input
          value={destination}
          onChange={(event) => {
            setDestination(event.currentTarget.value);
            setValidationError(null);
          }}
          placeholder="rmux-docker"
          autoFocus
        />
      </label>
      {warning ? (
        <small className="host-suggestion-warning" role="status">
          SSH config suggestions may be incomplete: {warning}
        </small>
      ) : null}
      {validationError ? <small>{validationError}</small> : null}
      <div className="form-actions">
        <button type="button" onClick={onClose}>
          Cancel
        </button>
        <button className="button-primary" type="submit">Add</button>
      </div>
    </form>
  );
}
