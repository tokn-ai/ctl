import { useEffect, useRef, useState } from "react";
import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import type { SavedTaskDefinition } from "../../lib/types";
import { QuickInputFrame } from "../commands/QuickInputFrame";

export function TaskEditor({
  model,
  saved,
}: {
  model: TaskWorkspace;
  saved?: SavedTaskDefinition;
}) {
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [instanceName, setInstanceName] = useState<string | null>(null);
  const previousFocus = useRef(document.activeElement);
  useEffect(
    () => () => {
      const element = previousFocus.current;
      requestAnimationFrame(() => {
        if (element instanceof HTMLElement && element.isConnected)
          element.focus();
      });
    },
    [],
  );
  const draft = model.draft;
  if (!draft) return null;
  const definition = draft.definition;
  const saveStatus = model.draftError
    ? "Draft not saved"
    : model.savingDraft
      ? "Saving draft…"
      : draft.dirty
        ? "Draft saved"
        : "Saved";
  return (
    <div className="task-dialog-layer">
      <QuickInputFrame
        title={saved ? "Edit task definition" : "Create task"}
        onDismiss={model.closeEditor}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            model.closeEditor();
          }
        }}
      >
        <div className="task-editor">
          <header className="task-page-heading">
            <div>
              <span className="task-eyebrow">WORKSPACE TASK · LOCAL</span>
              <h1>{saved ? "Edit task definition" : "Create task"}</h1>
              <p>
                Keep a command ready to run. Your draft saves automatically.
              </p>
            </div>
            <button
              className="task-dialog-close"
              type="button"
              aria-label="Close task editor"
              onClick={model.closeEditor}
            >
              ×
            </button>
          </header>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void model.save();
            }}
          >
            <fieldset disabled={model.busy}>
              <label>
                Command line
                <input
                  autoFocus
                  aria-label="Command line"
                  value={draft.command_line}
                  placeholder='cargo run --bin "api server"'
                  aria-invalid={!!draft.command_error}
                  aria-describedby="task-command-help"
                  onChange={(event) => model.editCommand(event.target.value)}
                />
                <small id="task-command-help">
                  POSIX quoting. Runs directly; no shell expansion.
                </small>
              </label>
              {draft.command_error ? (
                <p className="task-inline-error" role="alert">
                  {draft.command_error}
                </p>
              ) : null}
              <label>
                Name
                <input
                  value={definition.name}
                  placeholder="Optional: command-folder-random word"
                  onChange={(event) =>
                    model.edit({ ...definition, name: event.target.value })
                  }
                />
              </label>
              <div
                className="task-mode-picker"
                role="group"
                aria-label="Execution mode"
              >
                {(["background", "interactive"] as const).map((mode) => (
                  <button
                    type="button"
                    aria-pressed={definition.execution_mode === mode}
                    className={
                      definition.execution_mode === mode ? "chosen" : ""
                    }
                    key={mode}
                    onClick={() =>
                      model.edit({ ...definition, execution_mode: mode })
                    }
                  >
                    <strong>
                      {mode === "background" ? "Background" : "Interactive"}
                    </strong>
                    <span>
                      {mode === "background"
                        ? "Jobs and services with log output"
                        : "Commands that need a terminal"}
                    </span>
                  </button>
                ))}
              </div>
              <label>
                Executable
                <input
                  value={definition.program}
                  placeholder={
                    definition.execution_mode === "interactive"
                      ? "bash"
                      : "cargo"
                  }
                  onChange={(event) =>
                    model.edit({ ...definition, program: event.target.value })
                  }
                />
              </label>
              <div className="task-field-label">
                Arguments <small>One argument per row.</small>
              </div>
              {definition.arguments.map((argument, index) => (
                <div className="task-argument" key={index}>
                  <input
                    aria-label={`Argument ${index + 1}`}
                    value={argument}
                    onChange={(event) =>
                      model.edit({
                        ...definition,
                        arguments: definition.arguments.map(
                          (value, position) =>
                            position === index ? event.target.value : value,
                        ),
                      })
                    }
                  />
                  <button
                    type="button"
                    aria-label={`Remove argument ${index + 1}`}
                    onClick={() =>
                      model.edit({
                        ...definition,
                        arguments: definition.arguments.filter(
                          (_, position) => index !== position,
                        ),
                      })
                    }
                  >
                    ×
                  </button>
                </div>
              ))}
              <button
                className="task-text-button"
                type="button"
                onClick={() =>
                  model.edit({
                    ...definition,
                    arguments: [...definition.arguments, ""],
                  })
                }
              >
                + Add argument
              </button>
              <label>
                Working directory
                <input
                  value={definition.working_directory ?? ""}
                  placeholder="Absolute path (optional)"
                  onChange={(event) =>
                    model.edit({
                      ...definition,
                      working_directory: event.target.value || null,
                    })
                  }
                />
              </label>
              <div className="task-form-actions">
                <span role="status">{saveStatus}</span>
                <button type="submit">
                  {saved ? "Save changes" : "Create definition"}
                </button>
                <button
                  className="task-primary"
                  type="button"
                  onClick={() => void model.save(true)}
                >
                  {saved ? "Save and run" : "Create and run"}
                </button>
              </div>
            </fieldset>
          </form>
          {model.error || model.draftError ? (
            <div className="task-inline-error" role="alert">
              {model.error ?? model.draftError}
            </div>
          ) : null}
          {saved ? (
            <div className="task-secondary-actions">
              <button
                disabled={model.busy || draft.dirty}
                onClick={() => setInstanceName(`${saved.definition.name} (2)`)}
              >
                Run another instance…
              </button>
              <button
                className="task-danger"
                disabled={model.busy}
                onClick={() => setConfirmDelete(true)}
              >
                Delete definition…
              </button>
            </div>
          ) : null}
          {instanceName !== null && saved ? (
            <div className="task-confirm">
              <label>
                New task name
                <input
                  autoFocus
                  value={instanceName}
                  onChange={(event) => setInstanceName(event.target.value)}
                />
              </label>
              <button
                disabled={!instanceName.trim() || model.busy}
                onClick={() => {
                  void model.run(saved, true, instanceName);
                  setInstanceName(null);
                }}
              >
                Create and run
              </button>
              <button onClick={() => setInstanceName(null)}>Cancel</button>
            </div>
          ) : null}
          {confirmDelete && saved ? (
            <div className="task-confirm">
              <p>
                Delete “{saved.definition.name}” from this workspace? Its
                managed tasks will remain.
              </p>
              <button
                disabled={model.busy}
                onClick={() => void model.forgetDefinition(saved.definition_id)}
              >
                Delete definition
              </button>
              <button onClick={() => setConfirmDelete(false)}>Cancel</button>
            </div>
          ) : null}
        </div>
      </QuickInputFrame>
    </div>
  );
}
