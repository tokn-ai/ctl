import { useLocale } from "../../features/i18n/LocaleContext";
import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import type { SavedTaskDefinition, TaskReference } from "../../lib/types";
import { definitionScopeKey, definitionScopeLabel, GLOBAL_DEFINITION_SCOPE } from "../../features/tasks/useTaskDefinitions";
import { TaskDefinitionSource } from "./TaskDefinitionSource";
import { InlineFeedback } from "../feedback/InlineFeedback";
import { taskState } from "../../features/tasks/taskModel";
import { Icon } from "../ui/Icon";
export function TaskSidebar({
  model,
  definitions,
  references,
}: {
  model: TaskWorkspace;
  definitions: SavedTaskDefinition[];
  references: TaskReference[];
}) {
  const { t, locale, intl } = useLocale();
  return (
    <section className="task-sidebar" aria-label={t("tasks.title")}>
      <header>
        <strong>{t("tasks.title")}</strong>
        <button
          type="button"
          aria-label={t("tasks.refresh")}
          title={t("tasks.refresh")}
          disabled={model.loading || model.busy}
          onClick={() => void model.refresh()}
        >
          <Icon name="refresh" />
        </button>
        <button
          type="button"
          aria-label={t("tasks.new_definition")}
          title={t("tasks.new_definition")}
          disabled={model.busy}
          onClick={model.newDefinition}
        >
          <Icon name="plus" />
        </button>
      </header>
      <div className="task-browser">
        <div className="task-definitions">
          <TaskDefinitionSource model={model} />
          {model.drafts.filter(
            (draft) =>
              !definitions.some(
                (saved) => saved.definition_id === draft.definition_id && definitionScopeKey(draft.scope) === definitionScopeKey(model.definition_scope),
              ),
          ).length ? (
            <>
              <h3>{t("tasks.drafts")}</h3>
              {model.drafts
                .filter(
                  (draft) =>
                    !definitions.some(
                      (saved) => saved.definition_id === draft.definition_id && definitionScopeKey(draft.scope) === definitionScopeKey(model.definition_scope),
                    ),
                )
                .map((draft) => (
                  <div className="task-sidebar-row" key={`${definitionScopeKey(draft.scope)}:${draft.definition_id}`}>
                    <button
                      className="task-row-main"
                      aria-label={t("tasks.resume_draft", { name: draft.definition.name || t("tasks.untitled") })}
                      onClick={() => model.openEditor(draft.definition_id, draft.scope ?? GLOBAL_DEFINITION_SCOPE)}
                    >
                      <Icon name="tasks" class_name="task-definition-icon" />
                      <span>
                        {draft.definition.name || t("tasks.untitled")}
                        <small>{t("tasks.draft_scope", { scope: definitionScopeLabel(draft.scope ?? GLOBAL_DEFINITION_SCOPE, locale) })}</small>
                      </span>
                    </button>
                    <button
                      className="task-row-run"
                      aria-label={t("tasks.delete_draft", { name: draft.definition.name || t("tasks.untitled") })}
                      onClick={() => model.discardDraft(draft.definition_id, draft.scope ?? GLOBAL_DEFINITION_SCOPE)}
                    >
                      <Icon name="close" size={14} />
                    </button>
                  </div>
                ))}
            </>
          ) : null}
          <h3>{t("tasks.saved_definitions")}{" "}<span>{intl.formatNumber(definitions.length)}</span>
          </h3>
          {model.definitions_error ? <InlineFeedback className="task-feedback" action={{ label: t("tasks.retry_definitions"), on_click: () => void model.refreshDefinitions(), disabled: model.definitions_loading }}>
            {t("tasks.definitions_refresh_error", { error: model.definitions_error, stale: String(model.definitions_loaded) })}
          </InlineFeedback> : null}
          {!model.definitions_loaded && !model.definitions_error ? (
            <p className="task-sidebar-note">{t("tasks.loading_definitions")}</p>
          ) : model.definitions_loaded && definitions.length === 0 ? (
            <button className="task-empty-action" onClick={model.newDefinition}>
              <Icon name="plus" size={14} />{t("tasks.create_definition_action")}
            </button>
          ) : (
            definitions.map((saved) => (
              <div
                className={`task-sidebar-row ${model.editorId === saved.definition_id ? "selected" : ""}`}
                key={saved.definition_id}
              >
                <button
                  className="task-row-main"
                  onClick={() => model.openEditor(saved.definition_id, model.definition_scope)}
                >
                  <Icon name="tasks" class_name="task-definition-icon" />
                  <span>
                    {saved.definition.name}
                    <small>
                      {model.drafts.some(
                        (draft) => draft.definition_id === saved.definition_id && definitionScopeKey(draft.scope) === definitionScopeKey(model.definition_scope),
                      )
                        ? t("tasks.draft_changes")
                        : t(`tasks.state_${saved.definition.execution_mode}`)}
                    </small>
                  </span>
                </button>
                <button
                  className="task-row-run"
                  title={t("tasks.run_definition", { name: saved.definition.name })}
                  aria-label={t("tasks.run_definition", { name: saved.definition.name })}
                  disabled={model.busy}
                  onClick={() => void model.run(saved)}
                >
                  <Icon name="arrow_right" size={14} />
                </button>
              </div>
            ))
          )}
        </div>
        <div className="task-runs">
          <h3>{t("tasks.managed_tasks")}{" "}<span>{t("tasks.local")}</span>
          </h3>
          {model.connection_error ? <InlineFeedback className="task-feedback" action={{ label: t("tasks.retry_tasks"), on_click: () => void model.refresh(), disabled: model.loading }}>
            {t("tasks.refresh_error", { error: model.connection_error, stale: String(model.hasLoaded) })}
          </InlineFeedback> : null}
          {model.daemonStatus ? <span className="task-sidebar-note" role="status">{t("tasks.daemon_restarting")}</span> : null}
          {!model.hasLoaded && !model.connection_error ? (
            <p className="task-sidebar-note">{t("tasks.loading")}</p>
          ) : !model.tasks.length && !model.connection_error ? (
            <p className="task-sidebar-note">{t("tasks.empty_list")}
            </p>
          ) : null}
          {model.tasks.map((task) => (
            <button
              className={`task-sidebar-row task-row-main ${model.active?.kind === "task" && model.active.task_id === task.task_id ? "selected" : ""}`}
              key={task.task_id}
              onClick={() => model.openTask(task)}
            >
              <span className={`task-state-dot ${taskState(task)}`} />
              <span>
                {task.definition.name}
                <small>{t(`tasks.state_${taskState(task)}`)}</small>
              </span>
            </button>
          ))}
          {references
            .filter(
              (reference) =>
                reference.host_id !== "local" ||
                !model.tasks.some((task) => task.task_id === reference.task_id),
            )
            .map((reference) => (
              <button
                className="task-sidebar-row task-row-main"
                key={`${reference.host_id}:${reference.task_id}`}
                onClick={() =>
                  model.open({
                    kind: "task",
                    host_id: reference.host_id,
                    task_id: reference.task_id,
                  })
                }
              >
                <span className="task-state-dot unknown" />
                <span>
                  {model.savedForReference(reference)?.definition.name ?? t("tasks.saved_task")}
                  <small>
                    {reference.host_id === "local"
                      ? model.connection_error
                        ? t("tasks.unavailable")
                        : t("tasks.not_registered")
                      : t("tasks.remote_unsupported")}
                  </small>
                </span>
              </button>
            ))}
        </div>
      </div>
    </section>
  );
}
