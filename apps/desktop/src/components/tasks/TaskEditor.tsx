import { useLocale } from "../../features/i18n/LocaleContext";
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
  const { t, locale, formatText } = useLocale();
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
    return value === failure.field_value ? formatText(failure.text) : null;
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
    ? t("tasks.draft_unsaved")
    : model.savingDraft
      ? t("tasks.draft_saving")
      : draft.dirty
        ? t("tasks.draft_saved")
        : t("tasks.saved");
  return (
    <div className="task-dialog-layer">
      <QuickInputFrame
        title={saved ? t("tasks.edit_definition") : t("tasks.create_task")}
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
              <span className="task-eyebrow">{t("tasks.editor_eyebrow", { scope: definitionScopeLabel(draft.scope, locale) })}</span>
              <h1>{saved ? t("tasks.edit_definition") : t("tasks.create_task")}</h1>
              <p>{t("tasks.draft_help")}
              </p>
            </div>
            <button
              className="task-dialog-close"
              type="button"
              aria-label={t("tasks.close_editor")}
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
              <label>{t("tasks.command_line")}
                <input
                  autoFocus
                  aria-label={t("tasks.command_line")}
                  value={draft.command_line}
                  placeholder='cargo run --bin "api server"'
                  aria-invalid={!!draft.command_error}
                  aria-describedby={`task-command-help${draft.command_error ? ` ${fieldId("command_line")}` : ""}`}
                  onChange={(event) => model.editCommand(event.target.value)}
                />
                <small id="task-command-help">{t("tasks.command_quoting")}
                </small>
              </label>
              {fieldFeedback("command_line")}
              <label>{t("tasks.name")}
                <input
                  value={definition.name}
                  placeholder={t("tasks.optional_name")}
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
                aria-label={t("tasks.execution_mode")}
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
                      {mode === "background" ? t("tasks.background") : t("tasks.interactive")}
                    </strong>
                    <span>
                      {mode === "background"
                        ? t("tasks.background_hint")
                        : t("tasks.interactive_hint")}
                    </span>
                  </button>
                ))}
              </div>
              <label>{t("tasks.executable")}
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
              <div className="task-field-label">{t("tasks.arguments")}{" "}<small>{t("tasks.arguments_hint")}</small>
              </div>
              {definition.arguments.map((argument, index) => (
                <Fragment key={index}>
                  <div className="task-argument">
                    <input
                      aria-label={t("tasks.argument_number", { index: index + 1 })}
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
                      aria-label={t("tasks.remove_argument", { index: index + 1 })}
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
                <Icon name="plus" size={14} />{t("tasks.add_argument")}
              </button>
              <label>{t("tasks.working_directory")}
                <input
                  value={definition.working_directory ?? ""}
                  placeholder={t("tasks.optional_directory")}
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
                  {saved ? t("tasks.save_changes") : t("tasks.create_definition")}
                </button>
                <button
                  className="task-primary"
                  type="button"
                  onClick={() => void model.save(true)}
                >
                  {saved ? t("tasks.save_run") : t("tasks.create_run")}
                </button>
              </div>
            </fieldset>
          </form>
          {form_error ? (
            <InlineFeedback className="task-feedback">{formatText(form_error)}</InlineFeedback>
          ) : null}
          {model.draftConflict || model.draftReviewRequired ? (
            <InlineFeedback severity="warning" className="task-feedback">
              {model.draftReviewRequired
                ? t("tasks.review_draft")
                : t("tasks.draft_conflict")}
            </InlineFeedback>
          ) : null}
          {model.draftConflict || model.draftReviewRequired ? (
            <div className="task-secondary-actions">
              <button disabled={model.busy} onClick={() => setConfirmReload(true)}>{t("tasks.reload_latest")}</button>
              <button disabled={model.busy} onClick={() => void model.save()}>{t("tasks.retry_save")}</button>
            </div>
          ) : null}
          {confirmReload ? (
            <div className="task-confirm">
              <p>{t("tasks.replace_confirm")}</p>
              <button disabled={model.busy} onClick={() => { void model.reloadDraft(); setConfirmReload(false); }}>{t("tasks.replace_draft")}</button>
              <button onClick={() => setConfirmReload(false)}>{t("tasks.keep_draft")}</button>
            </div>
          ) : null}
          {saved ? (
            <div className="task-secondary-actions">
              <button
                disabled={model.busy || draft.dirty}
                onClick={() => setInstanceName(`${saved.definition.name} (2)`)}
              >{t("tasks.run_another")}
              </button>
              <button
                className="task-danger"
                disabled={model.busy}
                onClick={() => setConfirmDelete(true)}
              >{t("tasks.delete_definition_dialog")}
              </button>
            </div>
          ) : null}
          {instanceName !== null && saved ? (
            <div className="task-confirm">
              <label>{t("tasks.new_task_name")}
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
              >{t("tasks.create_run")}
              </button>
              <button onClick={() => setInstanceName(null)}>{t("tasks.cancel")}</button>
            </div>
          ) : null}
          {confirmDelete && saved ? (
            <div className="task-confirm">
              <p>
                {t("tasks.definition_changes_confirm", { name: saved.definition.name, scope: definitionScopeLabel(draft.scope, locale) })}
              </p>
              <button
                disabled={model.busy}
                onClick={() => void model.forgetDefinition(saved.definition_id)}
              >{t("tasks.delete_definition")}
              </button>
              <button onClick={() => setConfirmDelete(false)}>{t("tasks.cancel")}</button>
            </div>
          ) : null}
        </div>
      </QuickInputFrame>
    </div>
  );
}
