import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ManagedTask,
  SavedTaskDefinition,
  TaskTab,
  SessionSummary,
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
  const editor = useTaskEditor(workspace);
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
  ) => {
    const view = workspaceRef.current;
    let reference = another
      ? undefined
      : view.viewRef.current.task_references.find(
          (item) =>
            item.definition_id === definition.definition_id &&
            item.host_id === "local" &&
            item.is_default,
        );
    if (!reference) {
      reference = {
        host_id: "local",
        task_id: crypto.randomUUID(),
        definition_id: definition.definition_id,
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
    draft: editor.draft,
    saved: editor.saved,
    editorId: editor.definitionId,
    closeEditor: editor.close,
    openEditor: editor.open,
    drafts: workspace.task_drafts,
    savingDraft: workspace.saving,
    draftError: workspace.error,
    discardDraft: editor.discard,
    refresh,
    open,
    openTask,
    newDefinition: () => editor.create(),
    action,
    close: closeNow,
    closeNow,
    edit: editor.edit,
    save: (run = false) =>
      perform(async () => {
        const result = await editor.save();
        if (run) await startSaved(result);
        editor.close();
      }),
    run: (definition: SavedTaskDefinition, another = false, name?: string) =>
      perform(() => startSaved(definition, another, name)),
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
        editor.discard(definition_id);
        workspace.update("task_definitions", (definitions) =>
          definitions.filter((item) => item.definition_id !== definition_id),
        );
        workspace.update("task_references", (references) =>
          references.map((item) =>
            item.definition_id === definition_id
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
        const definition = workspace.task_definitions.find(
          (item) => item.definition_id === reference?.definition_id,
        );
        if (!definition)
          throw new Error("No saved definition is available for this task.");
        await taskRequest({
          type: "register_task",
          task_id,
          definition: definition.definition,
        });
      }),
  };
}
export type TaskWorkspace = ReturnType<typeof useTaskWorkspace>;
