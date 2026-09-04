import { useState } from "react";
import type { TaskDefinition, SavedTaskDefinition } from "../../lib/types";
import type { useWorkspace } from "../workspace/useWorkspace";
import { sameDefinition, validateDefinition } from "./taskModel";

import { parseCommandLine, formatCommandLine } from "./commandLine";

type Workspace = ReturnType<typeof useWorkspace>;

export function useTaskEditor(workspace: Workspace) {
  const [definitionId, setDefinitionId] = useState<string | null>(null);
  const saved = workspace.task_definitions.find(
    (item) => item.definition_id === definitionId,
  );
  const stored = workspace.task_drafts.find(
    (item) => item.definition_id === definitionId,
  );
  const source = stored?.definition ?? saved?.definition;
  const command_line =
    stored?.command_line ??
    (source ? formatCommandLine(source.program, source.arguments) : "");
  const parsed = parseCommandLine(command_line);
  const command_error = parsed.error;
  const definition = source
    ? {
        ...source,
        program: parsed.words[0] ?? "",
        arguments: parsed.words.slice(1),
      }
    : undefined;
  const draft = definition
    ? {
        definition,
        command_line,
        command_error,
        dirty:
          !!command_error ||
          !saved ||
          !sameDefinition(definition, saved.definition),
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

  function edit(definition: TaskDefinition, input?: string) {
    if (!definitionId) return;
    const commandChanged =
      !draft ||
      definition.program !== draft.definition.program ||
      JSON.stringify(definition.arguments) !==
        JSON.stringify(draft.definition.arguments);
    const command_line =
      input ??
      (commandChanged
        ? formatCommandLine(definition.program, definition.arguments)
        : draft.command_line);
    workspace.update("task_drafts", (drafts) => [
      ...drafts.filter((item) => item.definition_id !== definitionId),
      { definition_id: definitionId, definition, command_line },
    ]);
  }

  function editCommand(command_line: string) {
    if (!definition) return;
    const parsed = parseCommandLine(command_line);
    edit(
      {
        ...definition,
        program: parsed.words[0] ?? "",
        arguments: parsed.words.slice(1),
      },
      command_line,
    );
  }

  function close() {
    setDefinitionId(null);
    // A normal app close also flushes this same ordered writer.
    void workspace.persist().catch(() => undefined);
  }

  async function save(): Promise<SavedTaskDefinition> {
    if (!definitionId || !draft) throw new Error("No task draft is open.");
    const invalid = draft.command_error ?? validateDefinition(draft.definition);
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
    editCommand,
    close,
    save,
    discard,
  };
}
