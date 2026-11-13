import { Fragment, useEffect, useId, useRef, useState } from "react";
import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import type { SavedTaskDefinition } from "../../lib/types";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { definitionScopeLabel } from "../../features/tasks/useTaskDefinitions";
import { Icon } from "../ui/Icon";
import { InlineFeedback } from "../feedback/InlineFeedback";
import type { TaskDefinitionField } from "../../features/tasks/taskModel";

export function TaskEditor({
  model,
  saved,
}: {
  model: TaskWorkspace;
  saved?: SavedTaskDefinition;
}) {
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [confirmReload, setConfirmReload] = useState(false);
  const [instanceName, setInstanceName] = useState<string | null>(null);
  const feedback_id = useId();
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
  const command_error = draft.command_error;
  const form_error = model.editor_validation_error ? null : model.editor_error;
  function fieldError(field: TaskDefinitionField, argument_index?: number) {
    if (field === "command_line") return command_error;
    const failure = model.editor_validation_error;
    if (!failure || failure.field !== field || failure.argument_index !== argument_index) return null;
    const value = field === "arguments" ? definition.arguments[argument_index ?? -1] : definition[field] ?? "";
    return value === failure.field_value ? failure.message : null;
  }
  function fieldId(field: TaskDefinitionField, argument_index?: number) {
    return `${feedback_id}-${field}${argument_index === undefined ? "" : `-${argument_index}`}`;
  }
  function fieldAttributes(field: TaskDefinitionField, argument_index?: number) {
    const error = fieldError(field, argument_index);
    return { "aria-invalid": !!error, "aria-describedby": error ? fieldId(field, argument_index) : undefined };
  }
  function fieldFeedback(field: TaskDefinitionField, argument_index?: number) {
    const error = fieldError(field, argument_index);
    return error ? <div id={fieldId(field, argument_index)}><InlineFeedback className="task-feedback">{error}</InlineFeedback></div> : null;
  }
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
              <span className="task-eyebrow">TASK DEFINITION · {definitionScopeLabel(draft.scope)}</span>
              <h1>{saved ? "Edit task definition" : "Create task"}</h1>
              <p>
                Shared with ctl. Your draft saves automatically; Save updates the definition.
              </p>
            </div>
            <button
              className="task-dialog-close"
              type="button"
              aria-label="Close task editor"
              onClick={model.closeEditor}
            >
              <Icon name="close" />
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
                  aria-describedby={`task-command-help${draft.command_error ? ` ${fieldId("command_line")}` : ""}`}
                  onChange={(event) => model.editCommand(event.target.value)}
                />
                <small id="task-command-help">
                  POSIX quoting. Runs directly; no shell expansion.
                </small>
              </label>
              {fieldFeedback("command_line")}
              <label>
                Name
                <input
                  value={definition.name}
                  placeholder="Optional: command-folder-random word"
                  {...fieldAttributes("name")}
                  onChange={(event) =>
                    model.edit({ ...definition, name: event.target.value })
                  }
                />
              </label>
              {fieldFeedback("name")}
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
                  {...fieldAttributes("program")}
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
              {fieldFeedback("program")}
              <div className="task-field-label">
                Arguments <small>One argument per row.</small>
              </div>
              {definition.arguments.map((argument, index) => (
                <Fragment key={index}>
                  <div className="task-argument">
                    <input
                      aria-label={`Argument ${index + 1}`}
                      value={argument}
                      {...fieldAttributes("arguments", index)}
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
                      <Icon name="close" size={14} />
                    </button>
                  </div>
                  {fieldFeedback("arguments", index)}
                </Fragment>
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
                <Icon name="plus" size={14} /> Add argument
              </button>
              <label>
                Working directory
                <input
                  value={definition.working_directory ?? ""}
                  placeholder="Absolute path (optional)"
                  {...fieldAttributes("working_directory")}
                  onChange={(event) =>
                    model.edit({
                      ...definition,
                      working_directory: event.target.value || null,
                    })
                  }
                />
              </label>
              {fieldFeedback("working_directory")}
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
          {form_error ? (
            <InlineFeedback className="task-feedback">{form_error}</InlineFeedback>
          ) : null}
          {model.draftConflict || model.draftReviewRequired ? (
            <InlineFeedback severity="warning" className="task-feedback">
              {model.draftReviewRequired
                ? "This recovered draft needs review before replacing a shared definition."
                : "The shared definition changed outside this editor. Your draft and its original revision are kept."}
            </InlineFeedback>
          ) : null}
          {model.draftConflict || model.draftReviewRequired ? (
            <div className="task-secondary-actions">
              <button disabled={model.busy} onClick={() => setConfirmReload(true)}>Reload latest definition…</button>
              <button disabled={model.busy} onClick={() => void model.save()}>Retry save</button>
            </div>
          ) : null}
          {confirmReload ? (
            <div className="task-confirm">
              <p>Replace this draft with the latest saved definition? Your draft edits will be discarded.</p>
              <button disabled={model.busy} onClick={() => { void model.reloadDraft(); setConfirmReload(false); }}>Replace draft with latest</button>
              <button onClick={() => setConfirmReload(false)}>Keep draft</button>
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
                  void model.runFromEditor(saved, instanceName, draft.scope).then((complete) => {
                    if (complete) setInstanceName(null);
                  });
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
                Delete “{saved.definition.name}” from {definitionScopeLabel(draft.scope)} definitions? Its
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
