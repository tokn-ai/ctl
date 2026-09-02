import type { FormEvent } from "react";
import { useState } from "react";
import { errorMessage } from "../../lib/errors";
import type { SshHostDefinition, SshHostStorage } from "../../lib/types";

interface SshHostPickerProps {
  suggestions: readonly string[];
  warning: string | null;
  onActivateHost(destination: string): boolean;
  onSaveHost(
    definition: SshHostDefinition,
    storage: SshHostStorage,
  ): Promise<void>;
  onClose(): void;
}

interface HostFields {
  alias: string;
  hostname: string;
  user: string;
  port: string;
  identity_file: string;
}

interface SshHostStorageChoiceProps {
  definition: SshHostDefinition;
  saving: boolean;
  error: string | null;
  onSelect(storage: SshHostStorage): void;
  onBack(): void;
}

export function SshHostStorageChoice({
  definition,
  saving,
  error,
  onSelect,
  onBack,
}: SshHostStorageChoiceProps) {
  return (
    <div className="host-storage-step">
      <div>
        <strong>Where should this host be saved?</strong>
        <small>{definition.alias} · {definition.hostname}</small>
      </div>
      <button
        type="button"
        className="host-storage-choice"
        onClick={() => onSelect("ssh_config")}
        disabled={saving}
      >
        <strong>OpenSSH config</strong>
        <small>Reusable by ssh, ctl, and rmux-app.</small>
      </button>
      <button
        type="button"
        className="host-storage-choice"
        onClick={() => onSelect("local_storage")}
        disabled={saving}
      >
        <strong>This app only</strong>
        <small>Keep the connection settings in this WebView.</small>
      </button>
      {error ? (
        <small className="host-save-error" role="alert">{error}</small>
      ) : null}
      <div className="form-actions">
        <button type="button" onClick={onBack} disabled={saving}>
          Back
        </button>
        <span aria-live="polite">{saving ? "Saving…" : null}</span>
      </div>
    </div>
  );
}

export function parseSshHostDefinition(
  fields: HostFields,
): { definition: SshHostDefinition | null; error: string | null } {
  const alias = fields.alias.trim();
  const hostname = fields.hostname.trim();
  const user = fields.user.trim() || null;
  const identityFile = fields.identity_file.trim() || null;
  if (!isSafeToken(hostname)) {
    return { definition: null, error: "Enter a valid hostname or IP address." };
  }
  if (
    !isSafeToken(alias) ||
    alias.startsWith("!") ||
    alias.includes("*") ||
    alias.includes("?")
  ) {
    return { definition: null, error: "Enter a valid SSH alias." };
  }
  if (user && !isSafeToken(user)) {
    return { definition: null, error: "Enter a valid SSH user." };
  }
  if (identityFile && hasControlCharacter(identityFile)) {
    return { definition: null, error: "Enter a valid identity-file path." };
  }
  const port = fields.port.trim() ? Number(fields.port) : null;
  if (
    port !== null &&
    (!Number.isInteger(port) || port < 1 || port > 65_535)
  ) {
    return { definition: null, error: "Port must be between 1 and 65535." };
  }
  return {
    definition: {
      alias,
      hostname,
      user,
      port,
      identity_file: identityFile,
    },
    error: null,
  };
}

export function SshHostPicker({
  suggestions,
  warning,
  onActivateHost,
  onSaveHost,
  onClose,
}: SshHostPickerProps) {
  const [fields, setFields] = useState<HostFields>({
    alias: "",
    hostname: "",
    user: "",
    port: "",
    identity_file: "",
  });
  const [aliasEdited, setAliasEdited] = useState(false);
  const [definition, setDefinition] = useState<SshHostDefinition | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  function activateConfiguredHost(destination: string) {
    if (!onActivateHost(destination)) {
      setValidationError("That OpenSSH host is already active.");
      return;
    }
    onClose();
  }

  function submitDetails(event: FormEvent) {
    event.preventDefault();
    const parsed = parseSshHostDefinition(fields);
    setValidationError(parsed.error);
    if (parsed.definition) {
      setDefinition(parsed.definition);
    }
  }

  async function chooseStorage(storage: SshHostStorage) {
    if (!definition || saving) {
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      await onSaveHost(definition, storage);
      onClose();
    } catch (error) {
      setSaveError(errorMessage(error));
    } finally {
      setSaving(false);
    }
  }

  if (definition) {
    return (
      <div className="host-form">
        <SshHostStorageChoice
          definition={definition}
          saving={saving}
          error={saveError}
          onSelect={(storage) => void chooseStorage(storage)}
          onBack={() => {
            setDefinition(null);
            setSaveError(null);
          }}
        />
      </div>
    );
  }

  return (
    <form className="host-form" onSubmit={submitDetails}>
      {suggestions.length > 0 ? (
        <div className="host-suggestions">
          <span>Already in SSH config</span>
          <div>
            {suggestions.map((candidate) => (
              <button
                type="button"
                key={candidate}
                onClick={() => activateConfiguredHost(candidate)}
                aria-label={`Activate ${candidate}`}
                title={`Activate ${candidate}`}
              >
                {candidate}
              </button>
            ))}
          </div>
        </div>
      ) : null}
      <label>
        Hostname or IP address
        <input
          value={fields.hostname}
          onChange={(event) => {
            const hostname = event.currentTarget.value;
            setFields((current) => ({
              ...current,
              hostname,
              alias: aliasEdited ? current.alias : hostname,
            }));
            setValidationError(null);
          }}
          placeholder="127.0.0.1"
          autoFocus
        />
      </label>
      <label>
        Name / SSH alias
        <input
          value={fields.alias}
          onChange={(event) => {
            setAliasEdited(true);
            setFields((current) => ({
              ...current,
              alias: event.currentTarget.value,
            }));
            setValidationError(null);
          }}
          placeholder="rmux-remote-test"
        />
      </label>
      <details className="host-connection-details">
        <summary>Connection settings</summary>
        <label>
          User
          <input
            value={fields.user}
            onChange={(event) => {
              setFields((current) => ({
                ...current,
                user: event.currentTarget.value,
              }));
              setValidationError(null);
            }}
            placeholder="current user"
          />
        </label>
        <label>
          Port
          <input
            value={fields.port}
            inputMode="numeric"
            onChange={(event) => {
              setFields((current) => ({
                ...current,
                port: event.currentTarget.value,
              }));
              setValidationError(null);
            }}
            placeholder="22"
          />
        </label>
        <label>
          Identity file
          <input
            value={fields.identity_file}
            onChange={(event) => {
              setFields((current) => ({
                ...current,
                identity_file: event.currentTarget.value,
              }));
              setValidationError(null);
            }}
            placeholder="~/.ssh/id_ed25519"
          />
        </label>
      </details>
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
        <button className="button-primary" type="submit">Continue</button>
      </div>
    </form>
  );
}

function isSafeToken(value: string): boolean {
  return (
    Boolean(value) &&
    !hasControlCharacter(value) &&
    !/\s/u.test(value) &&
    !/[#\\'"=%]/u.test(value)
  );
}

function hasControlCharacter(value: string): boolean {
  return [...value].some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
  });
}
