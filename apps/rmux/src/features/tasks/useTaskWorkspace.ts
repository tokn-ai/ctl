import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ManagedTask,
  SavedTaskDefinition,
  TaskTab,
  SessionSummary,
  TaskDefinitionScope,
  TaskReference,
} from "../../lib/types";
import {
  inspectKnownSessions,
  restartTaskDaemon,
  taskRequest,
} from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type { useWorkspace } from "../workspace/useWorkspace";
import { workspaceTabKey } from "../workspace/workspaceModel";
import { sessionKey } from "../targets/targets";
import { sameDefinition } from "./taskModel";

import { useTaskEditor } from "./useTaskEditor";
import { definitionScopeKey, definitionScopeLabel, GLOBAL_DEFINITION_SCOPE, useTaskDefinitions } from "./useTaskDefinitions";

type Workspace = ReturnType<typeof useWorkspace>;
export function useTaskWorkspace(
  workspace: Workspace,
  connect: (session: SessionSummary) => Promise<void>,
  detach: () => Promise<void>,
) {
  const [tasks, setTasks] = useState<ManagedTask[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [hasLoaded, setHasLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [daemonStatus, setDaemonStatus] = useState<string | null>(null);
  const catalog = useTaskDefinitions(workspace.task_definition_scope, workspace.ready);
  const editor = useTaskEditor(workspace, catalog);
  useEffect(() => {
    if (definitionScopeKey(catalog.scope) !== definitionScopeKey(workspace.task_definition_scope))
      workspace.update("task_definition_scope", catalog.scope);
  }, [definitionScopeKey(catalog.scope), definitionScopeKey(workspace.task_definition_scope)]);
  const mutation = useRef(false);
  const epoch = useRef(0);
  const mounted = useRef(true);
  const loaded = useRef(false);
  const refreshing = useRef(false);
  const workspaceRef = useRef(workspace);
  workspaceRef.current = workspace;
  const connectRef = useRef(connect);
  connectRef.current = connect;
  const detachRef = useRef(detach);
  detachRef.current = detach;
  const active =
    workspace.task_tabs.find(
      (tab) => workspaceTabKey(tab) === workspace.active_tab_key,
    ) ?? null;
  const activeTask =
    active?.kind === "task"
      ? (tasks.find(
          (task) =>
            task.task_id === active.task_id && active.host_id === "local",
        ) ?? null)
      : null;
  const activeReference = active?.kind === "task" ? workspace.task_references.find((item) =>
    item.host_id === active.host_id && item.task_id === active.task_id) : undefined;
  const activeSaved = activeReference ? catalog.get(activeReference.definition_scope ?? GLOBAL_DEFINITION_SCOPE)?.definitions.find((item) =>
    item.definition_id === activeReference.definition_id) : undefined;
  useEffect(() => {
    if (!activeReference?.definition_id) return;
    const source = activeReference.definition_scope ?? GLOBAL_DEFINITION_SCOPE;
    if (definitionScopeKey(source) === definitionScopeKey(catalog.scope)) return;
    const refresh = () => { void catalog.load(source).catch((failure: unknown) => setError(errorMessage(failure))); };
    refresh();
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [activeReference?.definition_id, definitionScopeKey(activeReference?.definition_scope), definitionScopeKey(catalog.scope), catalog.load]);

  const refresh = useCallback(async (background = false) => {
    const version = epoch.current;
    if (mutation.current || refreshing.current) return;
    refreshing.current = true;
    if (!background) {
      setLoading(true);
      setError(null);
    }
    try {
      const result = await taskRequest({ type: "list_tasks" });
      if (
        mounted.current &&
        epoch.current === version &&
        result.type === "task_list"
      ) {
        setTasks(result.tasks);
        loaded.current = true;
        setHasLoaded(true);
        setRefreshError(null);
      }
    } catch (failure) {
      if (mounted.current && epoch.current === version)
        setRefreshError(errorMessage(failure));
    } finally {
      refreshing.current = false;
      if (mounted.current && !background) setLoading(false);
    }
  }, []);
  useEffect(() => {
    mounted.current = true;
    if (!workspace.ready) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      if (!loaded.current || document.visibilityState !== "hidden")
        await refresh(true);
      if (!stopped) timer = setTimeout(() => void poll(), 1500);
    };
    void poll();
    return () => {
      stopped = true;
      mounted.current = false;
      clearTimeout(timer);
    };
  }, [workspace.ready, refresh]);

  const perform = async (operation: () => Promise<void>) => {
    if (mutation.current) return;
    mutation.current = true;
    epoch.current += 1;
    setBusy(true);
    setError(null);
    setDaemonStatus(null);
    try {
      await operation();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      mutation.current = false;
      epoch.current += 1;
      if (mounted.current) {
        setBusy(false);
        void refresh(true);
      }
    }
  };
  const open = (tab: TaskTab) => {
    const view = workspaceRef.current;
    const key = workspaceTabKey(tab);
    view.update("task_tabs", (tabs) =>
      tabs.some((existing) => workspaceTabKey(existing) === key)
        ? tabs
        : [...tabs, tab],
    );
    view.update("tab_order", (order) =>
      order.includes(key) ? order : [...order, key],
    );
    view.setActiveTabKey(key);
  };
  const remember = (task: ManagedTask) => {
    const view = workspaceRef.current;
    view.update("task_references", (references) =>
      references.some(
        (reference) =>
          reference.task_id === task.task_id && reference.host_id === "local",
      )
        ? references
        : [
            ...references,
            {
              host_id: "local",
              task_id: task.task_id,
              definition_id: null,
              applied_revision: null,
              is_default: false,
            },
          ],
    );
  };
  const openTask = (task: ManagedTask) => {
    remember(task);
    const sessionId = task.active_run?.interactive?.session_id;
    const view = workspaceRef.current;
    const reference = view.task_references.find((item) => item.host_id === "local" && item.task_id === task.task_id);
    const source = reference?.definition_scope ?? GLOBAL_DEFINITION_SCOPE;
    if (reference?.definition_id && !catalog.get(source))
      void catalog.load(source).catch((failure: unknown) => setError(errorMessage(failure)));
    const oldTab = view.tabs.find(
      (tab) => tab.target.kind === "local" && tab.session_id === sessionId,
    );
    if (oldTab) {
      view.setTabs((tabs) =>
        tabs.filter((tab) => sessionKey(tab) !== sessionKey(oldTab)),
      );
      view.update("tab_order", (order) =>
        order.map((key) =>
          key === sessionKey(oldTab) ? `task:local:${task.task_id}` : key,
        ),
      );
    }
    open({ kind: "task", host_id: "local", task_id: task.task_id });
  };
  const startSaved = async (
    definition: SavedTaskDefinition,
    another = false,
    instanceName?: string,
    source: TaskDefinitionScope = catalog.scope,
  ) => {
    const view = workspaceRef.current;
    source = await catalog.ensureScope(source);
    // Resolve persisted aliases before default-instance lookup, including after an app restart.
    const candidates = view.viewRef.current.task_references.filter((item) =>
      !another && item.is_default && item.definition_id === definition.definition_id && item.host_id === "local");
    const canonicalReferences = await Promise.all(candidates.map(async (item) => ({
      ...item,
      definition_scope: await catalog.ensureScope(item.definition_scope),
    })));
    if (canonicalReferences.some((item, index) =>
      definitionScopeKey(item.definition_scope) !== definitionScopeKey(candidates[index].definition_scope))) {
      view.update("task_references", (references) => references.map((item) =>
        canonicalReferences.find((candidate) => candidate.task_id === item.task_id && candidate.host_id === item.host_id) ?? item));
      await view.persist();
    }
    let reference = another
      ? undefined
      : view.viewRef.current.task_references.find(
          (item) =>
            item.definition_id === definition.definition_id &&
            definitionScopeKey(catalog.resolveScope(item.definition_scope)) === definitionScopeKey(catalog.resolveScope(source)) &&
            item.host_id === "local" &&
            item.is_default,
        );
    if (!reference) {
      reference = {
        host_id: "local",
        task_id: crypto.randomUUID(),
        definition_id: definition.definition_id,
        definition_scope: source,
        applied_revision: definition.revision,
        is_default: !another,
      };
      view.update("task_references", (references) => [
        ...references,
        reference!,
      ]);
      await view.persist();
    }
    const result = await taskRequest({
      type: "register_task",
      task_id: reference.task_id,
      definition: {
        ...definition.definition,
        name: instanceName ?? definition.definition.name,
      },
    });
    if (result.type !== "task_created")
      throw new Error("Unexpected task registration response.");
    let task = result.task;
    setTasks((current) => [
      ...current.filter((item) => item.task_id !== task.task_id),
      task,
    ]);
    openTask(task);
    if (!another && !sameDefinition(task.definition, definition.definition))
      throw new Error(
        "Saved definition has changes. Apply them while stopped, or start the registered command.",
      );
    if (!task.active_run) {
      const started = await taskRequest({
        type: "start_task",
        task: task.task_id,
      });
      if (started.type === "task_status") {
        task = started.task;
        setTasks((current) => [
          ...current.filter((item) => item.task_id !== task.task_id),
          task,
        ]);
      }
    }
  };
  const action = (
    task: ManagedTask,
    type: "start_task" | "stop_task" | "restart_task" | "remove_task",
  ) =>
    perform(async () => {
      const response = await taskRequest({ type, task: task.task_id });
      if (response.type === "task_status")
        setTasks((current) =>
          current.map((item) =>
            item.task_id === task.task_id ? response.task : item,
          ),
        );
      if (response.type === "task_removed") {
        workspace.update("task_references", (references) =>
          references.filter((item) => item.task_id !== task.task_id),
        );
        closeNow(`task:local:${task.task_id}`);
        setTasks((current) =>
          current.filter((item) => item.task_id !== task.task_id),
        );
      }
    });
  const closeNow = (key: string) => {
    const view = workspaceRef.current;
    view.update("task_tabs", (tabs) =>
      tabs.filter((tab) => workspaceTabKey(tab) !== key),
    );
    view.update("tab_order", (order) => order.filter((item) => item !== key));
    if (view.active_tab_key === key) {
      view.setActiveTabKey(null);
      void detachRef
        .current()
        .catch((failure: unknown) => setError(errorMessage(failure)));
    }
  };
  const attachmentVersion = useRef(0);
  const desiredSession =
    activeTask?.active_run?.interactive?.session_id ?? null;
  useEffect(() => {
    const version = ++attachmentVersion.current;
    if (!active) return;
    void (async () => {
      try {
        await detachRef.current();
        if (
          !desiredSession ||
          active.kind !== "task" ||
          active.host_id !== "local"
        )
          return;
        const result = await inspectKnownSessions({ kind: "local" }, [
          desiredSession,
        ]);
        if (version !== attachmentVersion.current) return;
        if (!result[0]?.session)
          throw new Error("This task's terminal is no longer available.");
        await connectRef.current(result[0].session);
      } catch (failure) {
        if (version === attachmentVersion.current)
          setError(errorMessage(failure));
      }
    })();
    return () => {
      attachmentVersion.current += 1;
    };
  }, [workspace.active_tab_key, desiredSession]);

  const restartDaemon = () =>
    perform(async () => {
      setDaemonStatus("Restarting taskd…");
      try {
        await restartTaskDaemon();
        setRefreshError(null);
        setDaemonStatus("taskd restarted.");
      } catch (failure) {
        setDaemonStatus(null);
        throw failure;
      }
    });

  return {
    tasks,
    daemonStatus,
    restartDaemon,
    connection_error: refreshError,
    error: error ?? refreshError,
    setError,
    loading,
    hasLoaded,
    busy,
    active,
    activeTask,
    activeSaved,
    draft: editor.draft,
    saved: editor.saved,
    editorId: editor.definitionId,
    editorKey: `${definitionScopeKey(editor.scope)}:${editor.definitionId}`,
    closeEditor: editor.close,
    openEditor: editor.open,
    drafts: workspace.task_drafts,
    savingDraft: workspace.saving,
    draftError: workspace.error,
    discardDraft: editor.discard,
    definitions: catalog.definitions,
    definition_scope: catalog.scope,
    definition_path: catalog.path,
    definitions_loading: catalog.loading,
    definitions_loaded: catalog.has_loaded,
    definitions_error: catalog.error,
    refreshDefinitions: catalog.refresh,
    selectDefinitionScope: (source: TaskDefinitionScope) => workspace.update("task_definition_scope", source),
    savedForReference: (reference: TaskReference) => catalog.get(reference.definition_scope ?? GLOBAL_DEFINITION_SCOPE)?.definitions.find((item) => item.definition_id === reference.definition_id),
    draftConflict: editor.conflict,
    draftReviewRequired: editor.reviewRequired,
    reloadDraft: () => perform(editor.reload),
    refresh: async () => { await Promise.all([refresh(), catalog.refresh()]); },
    open,
    openTask,
    newDefinition: () => editor.create(),
    action,
    close: closeNow,
    closeNow,
    edit: editor.edit,
    editCommand: editor.editCommand,
    save: (run = false) =>
      perform(async () => {
        const result = await editor.save();
        if (run) await startSaved(result, false, undefined, editor.scope);
        editor.close();
      }),
    run: (definition: SavedTaskDefinition, another = false, name?: string, source?: TaskDefinitionScope) =>
      perform(() => startSaved(definition, another, name, source)),
    saveAsDefinition: (task: ManagedTask) => editor.create(task.definition),
    apply: (task: ManagedTask, definition: SavedTaskDefinition) =>
      perform(async () => {
        await taskRequest({
          type: "update_task",
          task: task.task_id,
          definition: definition.definition,
        });
        workspace.update("task_references", (references) =>
          references.map((item) =>
            item.task_id === task.task_id
              ? { ...item, applied_revision: definition.revision }
              : item,
          ),
        );
        await workspace.persist();
      }),
    forgetDefinition: (definition_id: string) =>
      perform(async () => {
        const source = editor.definitionId === definition_id ? editor.scope : catalog.scope;
        const saved = catalog.get(source)?.definitions.find((item) => item.definition_id === definition_id);
        const expected_revision = editor.definitionId === definition_id ? editor.draft?.base_revision : saved?.revision;
        if (typeof expected_revision !== "string") throw new Error("Reload the saved definition before deleting it.");
        await catalog.remove(source, definition_id, expected_revision);
        editor.discard(definition_id, source);
        workspace.update("task_references", (references) =>
          references.map((item) =>
            item.definition_id === definition_id && definitionScopeKey(item.definition_scope) === definitionScopeKey(source)
              ? {
                  ...item,
                  definition_id: null,
                  applied_revision: null,
                  is_default: false,
                }
              : item,
          ),
        );
        await workspace.persist();
      }),
    recreate: (task_id: string) =>
      perform(async () => {
        const reference = workspace.task_references.find(
          (item) => item.task_id === task_id,
        );
        const source = reference?.definition_scope ?? GLOBAL_DEFINITION_SCOPE;
        const latest = await catalog.load(source);
        const definition = latest.definitions.find(
          (item) => item.definition_id === reference?.definition_id,
        );
        if (!definition)
          throw new Error(`No saved definition is available for this task in ${definitionScopeLabel(source)}.`);
        await taskRequest({
          type: "register_task",
          task_id,
          definition: definition.definition,
        });
      }),
  };
}
export type TaskWorkspace = ReturnType<typeof useTaskWorkspace>;
