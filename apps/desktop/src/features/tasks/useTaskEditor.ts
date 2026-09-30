import { useEffect, useRef, useState } from "react";
import type { TaskDefinition, SavedTaskDefinition, TaskDefinitionScope, TaskDefinitionDraft } from "../../lib/types";
import type { useWorkspace } from "../workspace/useWorkspace";
import { generateTaskName } from "./taskName";
import { sameDefinition, validateDefinition } from "./taskModel";

import { parseCommandLine, formatCommandLine } from "./commandLine";
import { definitionScopeKey, type TaskDefinitions } from "./useTaskDefinitions";

type Workspace = ReturnType<typeof useWorkspace>;

export function useTaskEditor(workspace: Workspace, catalog: TaskDefinitions) {
  const [selection, setSelection] = useState<{ definition_id: string; scope: TaskDefinitionScope } | null>(null);
  const selectionRef = useRef(selection);
  const definitionId = selection?.definition_id ?? null;
  const scope = selection?.scope ?? catalog.scope;
  const sourceCatalog = catalog.get(scope);
  const saved = sourceCatalog?.definitions.find((item) => item.definition_id === definitionId);
  const stored = workspace.task_drafts.find(
    (item) => item.definition_id === definitionId && definitionScopeKey(item.scope) === definitionScopeKey(scope),
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
        scope,
        base_revision: stored?.base_revision,
        dirty:
          !!command_error ||
          !saved ||
          !sameDefinition(definition, saved.definition),
      }
    : null;

  const reviewRequired = !!stored && stored.base_revision === undefined && !!saved;
  const conflict = !!stored && !!sourceCatalog &&
    stored.base_revision !== undefined && stored.base_revision !== (saved?.revision ?? null);

  useEffect(() => {
    if (!selection) return;
    const refresh = () => { void catalog.load(scope).catch(() => undefined); };
    if (!sourceCatalog) refresh();
    if (definitionScopeKey(scope) === definitionScopeKey(catalog.scope)) return;
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [selection?.definition_id, definitionScopeKey(scope), definitionScopeKey(catalog.scope), catalog.load]);

  function replaceDraft(value: TaskDefinitionDraft) {
    workspace.update("task_drafts", (drafts) => [
      ...drafts.filter((item) => item.definition_id !== value.definition_id || definitionScopeKey(item.scope) !== definitionScopeKey(value.scope)),
      value,
    ]);
  }

  function open(id: string, source: TaskDefinitionScope = catalog.scope) {
    workspace.update("sidebar_view", "tasks");
    const existing = workspace.viewRef.current.task_drafts.find((item) =>
      item.definition_id === id && definitionScopeKey(item.scope) === definitionScopeKey(source));
    const snapshot = catalog.get(source)?.definitions.find((item) => item.definition_id === id);
    if (!existing && snapshot) replaceDraft({
      definition_id: id,
      definition: snapshot.definition,
      scope: source,
      base_revision: snapshot.revision,
    });
    selectionRef.current = { definition_id: id, scope: source };
    setSelection(selectionRef.current);
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
    replaceDraft({ definition_id, definition, scope: catalog.scope, base_revision: null });
    selectionRef.current = { definition_id, scope: catalog.scope };
    setSelection(selectionRef.current);
    workspace.update("sidebar_view", "tasks");
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
    replaceDraft({
      ...stored,
      definition_id: definitionId,
      definition,
      command_line,
      scope,
      // Never substitute a catalog revision for the revision this draft began with.
      ...(stored ? {} : { base_revision: saved?.revision ?? null }),
    });
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
    // Async save/run completion may belong to a dialog the user has since replaced.
    if (selectionRef.current !== selection) return;
    if (definitionId && draft && !draft.dirty && !conflict && !reviewRequired)
      workspace.update("task_drafts", (drafts) => drafts.filter((item) =>
        item.definition_id !== definitionId || definitionScopeKey(item.scope) !== definitionScopeKey(scope)));
    selectionRef.current = null;
    setSelection(null);
    // A normal app close also flushes this same ordered writer.
    void workspace.persist().catch(() => undefined);
  }

  async function save(): Promise<SavedTaskDefinition> {
    if (!definitionId || !draft) throw new Error("No task draft is open.");
    if (reviewRequired) throw new Error("This recovered draft has no original revision. Your draft is kept; reload the saved definition before saving, or keep it for later review.");
    const definition = {
      ...draft.definition,
      name: draft.definition.name.trim()
        ? draft.definition.name
        : generateTaskName(draft.definition),
    };
    const invalid = draft.command_error ?? validateDefinition(definition);
    if (invalid) throw new Error(invalid);
    if (definition.name !== draft.definition.name) edit(definition);
    // Persist the draft first so a catalog or workspace write failure remains recoverable.
    await workspace.persist();
    const value = await catalog.save(scope, definitionId, stored?.base_revision ?? null, definition);
    workspace.update("task_drafts", (drafts) =>
      drafts.filter((item) => item.definition_id !== definitionId || definitionScopeKey(item.scope) !== definitionScopeKey(scope)),
    );
    await workspace.persist();
    return value;
  }

  async function reload() {
    if (!definitionId) return;
    const latest = await catalog.load(scope);
    const snapshot = latest.definitions.find((item) => item.definition_id === definitionId);
    if (!snapshot) throw new Error("The saved definition no longer exists. Your draft has been kept.");
    replaceDraft({ definition_id: definitionId, definition: snapshot.definition, scope, base_revision: snapshot.revision });
    await workspace.persist();
  }

  function discard(id: string, source: TaskDefinitionScope = scope) {
    workspace.update("task_drafts", (drafts) =>
      drafts.filter((item) => item.definition_id !== id || definitionScopeKey(item.scope) !== definitionScopeKey(source)),
    );
    if (definitionId === id && definitionScopeKey(source) === definitionScopeKey(scope)) close();
  }

  return {
    definitionId,
    saved,
    draft,
    scope,
    conflict,
    reviewRequired,
    reload,
    open,
    create,
    edit,
    editCommand,
    close,
    save,
    discard,
  };
}
