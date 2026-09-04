import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import type { SavedTaskDefinition, TaskReference } from "../../lib/types";
import { definitionScopeKey, definitionScopeLabel, GLOBAL_DEFINITION_SCOPE } from "../../features/tasks/useTaskDefinitions";
import { TaskDefinitionSource } from "./TaskDefinitionSource";
import { taskState } from "../../features/tasks/taskModel";
export function TaskSidebar({
  model,
  definitions,
  references,
}: {
  model: TaskWorkspace;
  definitions: SavedTaskDefinition[];
  references: TaskReference[];
}) {
  return (
    <section className="task-sidebar" aria-label="Tasks">
      <header>
        <strong>Tasks</strong>
        <button
          type="button"
          aria-label="Refresh tasks"
          title="Refresh tasks"
          disabled={model.loading || model.busy}
          onClick={() => void model.refresh()}
        >
          ↻
        </button>
        <button
          type="button"
          aria-label="New task definition"
          title="New task definition"
          disabled={model.busy}
          onClick={model.newDefinition}
        >
          +
        </button>
      </header>
      <TaskDefinitionSource model={model} />
      {model.drafts.filter(
        (draft) =>
          !definitions.some(
            (saved) => saved.definition_id === draft.definition_id && definitionScopeKey(draft.scope) === definitionScopeKey(model.definition_scope),
          ),
      ).length ? (
        <>
          <h3>Drafts</h3>
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
                  aria-label={`Resume draft ${draft.definition.name || "Untitled task"}`}
                  onClick={() => model.openEditor(draft.definition_id, draft.scope ?? GLOBAL_DEFINITION_SCOPE)}
                >
                  <span aria-hidden="true">◇</span>
                  <span>
                    {draft.definition.name || "Untitled task"}
                    <small>Draft · {definitionScopeLabel(draft.scope ?? GLOBAL_DEFINITION_SCOPE)}</small>
                  </span>
                </button>
                <button
                  className="task-row-run"
                  aria-label={`Delete draft ${draft.definition.name || "Untitled task"}`}
                  onClick={() => model.discardDraft(draft.definition_id, draft.scope ?? GLOBAL_DEFINITION_SCOPE)}
                >
                  ×
                </button>
              </div>
            ))}
        </>
      ) : null}
      <h3>
        Saved definitions <span>{definitions.length}</span>
      </h3>
      {!model.definitions_loaded && !model.definitions_error ? (
        <p className="task-sidebar-note">Loading definitions…</p>
      ) : model.definitions_loaded && definitions.length === 0 ? (
        <button className="task-empty-action" onClick={model.newDefinition}>
          + Create a task definition
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
              <span aria-hidden="true">◇</span>
              <span>
                {saved.definition.name}
                <small>
                  {model.drafts.some(
                    (draft) => draft.definition_id === saved.definition_id && definitionScopeKey(draft.scope) === definitionScopeKey(model.definition_scope),
                  )
                    ? "Draft changes"
                    : saved.definition.execution_mode}
                </small>
              </span>
            </button>
            <button
              className="task-row-run"
              title={`Run ${saved.definition.name}`}
              aria-label={`Run ${saved.definition.name}`}
              disabled={model.busy}
              onClick={() => void model.run(saved)}
            >
              ▷
            </button>
          </div>
        ))
      )}
      {model.definitions_error ? (
        <div className="task-inline-error" role="alert">
          Could not load definitions: {model.definitions_error}
          <button disabled={model.definitions_loading} onClick={() => void model.refreshDefinitions()}>Retry definitions</button>
        </div>
      ) : null}
      <h3>
        Managed tasks <span>Local</span>
      </h3>
      {!model.hasLoaded && !model.error ? (
        <p className="task-sidebar-note">Loading tasks…</p>
      ) : !model.tasks.length && !model.error ? (
        <p className="task-sidebar-note">
          Run a saved definition, or create a task with ctl.
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
            <small>{taskState(task)}</small>
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
              {model.savedForReference(reference)?.definition.name ?? "Saved task"}
              <small>
                {reference.host_id === "local"
                  ? model.connection_error
                    ? "Unavailable"
                    : "Not registered"
                  : "Remote · unsupported"}
              </small>
            </span>
          </button>
        ))}
      {model.daemonStatus ? (
        <p className="task-sidebar-note" role="status">
          {model.daemonStatus}
        </p>
      ) : null}
      {model.error ? (
        <div className="task-inline-error" role="alert">
          {model.error}
          <button onClick={() => void model.refresh()}>Retry</button>
        </div>
      ) : null}
    </section>
  );
}
