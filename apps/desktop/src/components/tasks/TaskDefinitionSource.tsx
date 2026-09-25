import { useEffect, useState } from "react";
import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import { definitionScopeKey, validProjectRoot } from "../../features/tasks/useTaskDefinitions";

export function TaskDefinitionSource({ model }: { model: TaskWorkspace }) {
  const scope = model.definition_scope;
  const [kind, setKind] = useState(scope.kind);
  const [projectRoot, setProjectRoot] = useState(scope.kind === "project" ? scope.project_root : "");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setKind(scope.kind);
    if (scope.kind === "project") setProjectRoot(scope.project_root);
    setError(null);
  }, [definitionScopeKey(scope)]);

  return (
    <form className="task-definition-source" onSubmit={(event) => {
      event.preventDefault();
      if (!validProjectRoot(projectRoot)) {
        setError("Enter an absolute project folder path.");
        return;
      }
      setError(null);
      model.selectDefinitionScope({ kind: "project", project_root: projectRoot });
    }}>
      <label>
        Definition source
        <select aria-label="Definition source" value={kind} disabled={model.busy} onChange={(event) => {
          const next = event.target.value as "global" | "project";
          setKind(next);
          setError(null);
          if (next === "global") model.selectDefinitionScope({ kind: "global" });
        }}>
          <option value="global">Global</option>
          <option value="project">Project</option>
        </select>
      </label>
      {kind === "project" ? (
        <>
          <label>
            Project folder
            <input aria-label="Project folder" value={projectRoot} placeholder="/absolute/project/path" disabled={model.busy}
              onChange={(event) => setProjectRoot(event.target.value)} />
          </label>
          <button disabled={model.busy} type="submit">Open project definitions</button>
          {scope.kind !== "project" ? <small>Global definitions remain selected until you open a project.</small> : null}
        </>
      ) : null}
      {error ? <p className="task-inline-error" role="alert">{error}</p> : null}
      {model.definition_path ? <small className="task-definition-path" title={model.definition_path}>{model.definition_path}</small> : null}
    </form>
  );
}
