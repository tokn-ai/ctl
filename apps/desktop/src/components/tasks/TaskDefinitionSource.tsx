import { message, type LocalizedMessage } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useState } from "react";
import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import { definitionScopeKey, validProjectRoot } from "../../features/tasks/useTaskDefinitions";
import { InlineFeedback } from "../feedback/InlineFeedback";

export function TaskDefinitionSource({ model }: { model: TaskWorkspace }) {
  const { t, formatText } = useLocale();
  const scope = model.definition_scope;
  const [kind, setKind] = useState(scope.kind);
  const [projectRoot, setProjectRoot] = useState(scope.kind === "project" ? scope.project_root : "");
  const [error, setError] = useState<LocalizedMessage | null>(null);
  useEffect(() => {
    setKind(scope.kind);
    if (scope.kind === "project") setProjectRoot(scope.project_root);
    setError(null);
  }, [definitionScopeKey(scope)]);

  return (
    <form className="task-definition-source" onSubmit={(event) => {
      event.preventDefault();
      if (!validProjectRoot(projectRoot)) {
        setError(message('tasks.invalid_project'));
        return;
      }
      setError(null);
      model.selectDefinitionScope({ kind: "project", project_root: projectRoot });
    }}>
      <label>{t("tasks.definition_source")}
        <select aria-label={t("tasks.definition_source")} value={kind} disabled={model.busy} onChange={(event) => {
          const next = event.target.value as "global" | "project";
          setKind(next);
          setError(null);
          if (next === "global") model.selectDefinitionScope({ kind: "global" });
        }}>
          <option value="global">{t("tasks.global")}</option>
          <option value="project">{t("tasks.project")}</option>
        </select>
      </label>
      {kind === "project" ? (
        <>
          <label>{t("tasks.project_folder")}
            <input aria-label={t("tasks.project_folder")} value={projectRoot} placeholder="/absolute/project/path" disabled={model.busy}
              onChange={(event) => setProjectRoot(event.target.value)} />
          </label>
          <button disabled={model.busy} type="submit">{t("tasks.open_project")}</button>
          {scope.kind !== "project" ? <small>{t("tasks.project_selection_hint")}</small> : null}
        </>
      ) : null}
      {error ? <InlineFeedback className="task-feedback">{formatText(error)}</InlineFeedback> : null}
      {model.definition_path ? <small className="task-definition-path" title={model.definition_path}>{model.definition_path}</small> : null}
    </form>
  );
}
