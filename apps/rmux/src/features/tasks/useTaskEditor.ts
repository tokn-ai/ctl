import { useState } from "react";
import type { TaskDefinition, SavedTaskDefinition } from "../../lib/types";
import type { useWorkspace } from "../workspace/useWorkspace";
import { sameDefinition, validateDefinition } from "./taskModel";

type Workspace = ReturnType<typeof useWorkspace>;

export function useTaskEditor(workspace: Workspace) {
  const [definitionId, setDefinitionId] = useState<string | null>(null);
  const saved = workspace.task_definitions.find(
    (item) => item.definition_id === definitionId,
  );
  const stored = workspace.task_drafts.find(
    (item) => item.definition_id === definitionId,
  );
  const definition = stored?.definition ?? saved?.definition;
  const draft = definition
    ? {
        definition,
        dirty: !saved || !sameDefinition(definition, saved.definition),
      }
    : null;

  function open(id: string) {
    workspace.update("sidebar_view", "tasks");
    setDefinitionId(id);
  }

  function create(
    definition: TaskDefinition = {
      name: "",
      program: "",
      arguments: [],
      working_directory: null,
      execution_mode: "background",
    },
  ) {
    const definition_id = crypto.randomUUID();
    workspace.update("task_drafts", (drafts) => [
      ...drafts,
      { definition_id, definition },
    ]);
    open(definition_id);
  }

  function edit(definition: TaskDefinition) {
    if (!definitionId) return;
    workspace.update("task_drafts", (drafts) => [
      ...drafts.filter((item) => item.definition_id !== definitionId),
      { definition_id: definitionId, definition },
    ]);
  }

  function close() {
    setDefinitionId(null);
    // A normal app close also flushes this same ordered writer.
    void workspace.persist().catch(() => undefined);
  }

  async function save(): Promise<SavedTaskDefinition> {
    if (!definitionId || !draft) throw new Error("No task draft is open.");
    const invalid = validateDefinition(draft.definition);
    if (invalid) throw new Error(invalid);
    const value = {
      definition_id: definitionId,
      definition: draft.definition,
      revision: crypto.randomUUID(),
    };
    workspace.update("task_definitions", (definitions) => [
      ...definitions.filter((item) => item.definition_id !== definitionId),
      value,
    ]);
    await workspace.persist();
    workspace.update("task_drafts", (drafts) =>
      drafts.filter((item) => item.definition_id !== definitionId),
    );
    await workspace.persist();
    return value;
  }

  function discard(id: string) {
    workspace.update("task_drafts", (drafts) =>
      drafts.filter((item) => item.definition_id !== id),
    );
    if (definitionId === id) close();
  }

  return {
    definitionId,
    saved,
    draft,
    open,
    create,
    edit,
    close,
    save,
    discard,
  };
}
