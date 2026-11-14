import { message } from "../i18n/messages";
// @vitest-environment jsdom
import { act, cleanup, render, renderHook, screen, waitFor } from "@testing-library/react";
import { useRef, useState, type PropsWithChildren, type SetStateAction } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useTaskWorkspace } from "./useTaskWorkspace";
import { TaskSidebar } from "../../components/tasks/TaskSidebar";
import { TaskEditor } from "../../components/tasks/TaskEditor";
import userEvent from "@testing-library/user-event";
import { emptyWorkspaceView, type WorkspaceView } from "../workspace/workspaceModel";
import type { useWorkspace } from "../workspace/useWorkspace";
import type { ManagedTask, TaskDefinition, TaskDefinitionCatalog, TaskDefinitionScope, TaskRequest } from "../../lib/types";
import { NotificationProvider } from "../notifications/NotificationContext";
import { NotificationStore } from "../notifications/NotificationStore";
const api = vi.hoisted(() => ({ taskRequest: vi.fn(), loadTaskDefinitions: vi.fn(), saveTaskDefinition: vi.fn(), removeTaskDefinition: vi.fn(), restartTaskDaemon: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ ...api, inspectKnownSessions: vi.fn() }));
afterEach(() => { cleanup(); vi.useRealTimers(); vi.resetAllMocks(); });

function useTestWorkspace(initial: WorkspaceView, persist_failure?: Error) {
  const [view, setView] = useState(initial);
  const viewRef = useRef(view);
  const persisted = useRef<WorkspaceView[]>([]);
  function update<K extends keyof WorkspaceView>(key: K, value: SetStateAction<WorkspaceView[K]>) {
    const next = typeof value === "function"
      ? (value as (current: WorkspaceView[K]) => WorkspaceView[K])(viewRef.current[key]) : value;
    viewRef.current = { ...viewRef.current, [key]: next };
    setView(viewRef.current);
  }
  const workspace = {
    ...view, viewRef, ready: true, update,
    persist: async () => {
      persisted.current.push(viewRef.current);
      if (persist_failure) throw persist_failure;
    },
    setActiveTabKey: (key: string | null) => update("active_tab_key", key),
  } as unknown as ReturnType<typeof useWorkspace>;
  return { model: useTaskWorkspace(workspace, async () => {}, async () => {}), view, persisted };
}

const alias: TaskDefinitionScope = { kind: "project", project_root: "/work/link" };
const canonical: TaskDefinitionScope = { kind: "project", project_root: "/work/project" };
const definition: TaskDefinition = { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" };
const task: ManagedTask = { task_id: "build-task", definition, desired_state: "stopped", active_run: null, last_run: null };

function notificationWrapper(store: NotificationStore) {
  return function Wrapper({ children }: PropsWithChildren) {
    return <NotificationProvider store={store}>{children}</NotificationProvider>;
  };
}

function mockAvailableTasks() {
  api.loadTaskDefinitions.mockResolvedValue({ scope: { kind: "global" }, path: "/test/definitions.json", definitions: [] });
  api.taskRequest.mockResolvedValue({ type: "task_list", tasks: [] });
}

function mockTaskRegistration() {
  api.taskRequest.mockImplementation(async (request: TaskRequest) => {
    if (request.type === "register_task") return {
      type: "task_created",
      task: { task_id: request.task_id, definition: request.definition, desired_state: "stopped", active_run: null, last_run: null },
    };
    return { type: "task_list", tasks: [] };
  });
}

describe("task background refresh", () => {
  it("shares the task mutation guard with About actions in both directions", async () => {
    api.loadTaskDefinitions.mockResolvedValue({ scope: { kind: "global" }, path: "/test/definitions.json", definitions: [] });
    api.taskRequest.mockResolvedValue({ type: "task_list", tasks: [] });
    let finish!: () => void;
    api.restartTaskDaemon.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()));
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    act(() => { void result.current.model.restartDaemon(); });
    const execute = vi.fn(async () => "restarted");
    await act(async () => { await expect(result.current.model.performComponentAction(execute)).rejects.toThrow("A task operation is already in progress"); });
    expect(execute).not.toHaveBeenCalled();
    await act(async () => { finish(); });
    let complete!: (value: string) => void;
    execute.mockImplementationOnce(() => new Promise<string>((resolve) => { complete = resolve; }));
    let pending!: Promise<string>;
    act(() => { pending = result.current.model.performComponentAction(execute); });
    await act(async () => { await result.current.model.restartDaemon(); });
    expect(api.restartTaskDaemon).toHaveBeenCalledOnce();
    expect(result.current.model.busy).toBe(true);
    await act(async () => { complete("restarted"); await expect(pending).resolves.toBe("restarted"); });
    expect(result.current.model.busy).toBe(false);
  });

  it("propagates About failures and releases the task guard for another action", async () => {
    api.loadTaskDefinitions.mockResolvedValue({ scope: { kind: "global" }, path: "/test/definitions.json", definitions: [] });
    api.taskRequest.mockResolvedValue({ type: "task_list", tasks: [] });
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()));
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    await act(async () => { await expect(result.current.model.performComponentAction(async () => { throw new Error("Active task prevents restart."); })).rejects.toThrow("Active task prevents restart."); });
    expect(result.current.model.error).toBe("Active task prevents restart.");
    expect(result.current.model.busy).toBe(false);
    await act(async () => { await expect(result.current.model.performComponentAction(async () => "complete")).resolves.toBe("complete"); });
  });

  it("keeps the empty state and refresh control stable while a poll is pending", async () => {
    vi.useFakeTimers();
    const view = emptyWorkspaceView();
    api.loadTaskDefinitions.mockResolvedValue({ scope: { kind: "global" }, path: "/test/definitions.json", definitions: [] });
    const workspace = { ...view, ready: true, viewRef: { current: view } } as unknown as ReturnType<typeof useWorkspace>;
    let resolvePoll!: (value: { type: string; tasks: never[] }) => void;
    api.taskRequest.mockResolvedValueOnce({ type: "task_list", tasks: [] }).mockImplementationOnce(() => new Promise((resolve) => { resolvePoll = resolve; }));
    function TestView() {
      const model = useTaskWorkspace(workspace, async () => {}, async () => {});
      return <TaskSidebar model={model} definitions={[]} references={[]} />;
    }
    await act(async () => { render(<TestView />); });
    const empty = screen.getByText("Run a saved definition, or create a task with ctl.");
    await act(async () => { vi.advanceTimersByTime(1500); });
    expect(api.taskRequest).toHaveBeenCalledTimes(2);
    expect(screen.getByText("Run a saved definition, or create a task with ctl.")).toBe(empty);
    expect(screen.queryByText("Loading tasks…")).toBeNull();
    expect((screen.getByRole("button", { name: "Refresh tasks" }) as HTMLButtonElement).disabled).toBe(false);
    await act(async () => { resolvePoll({ type: "task_list", tasks: [] }); });
    expect(screen.getByText("Run a saved definition, or create a task with ctl.")).toBe(empty);
  });
});

describe("task message ownership", () => {
  it("keeps local validation at its field and treats identical backend text as a form error", async () => {
    mockAvailableTasks();
    api.saveTaskDefinition.mockRejectedValueOnce(new Error("Use an absolute working directory."));
    const store = new NotificationStore();
    function Editor() {
      const { model } = useTestWorkspace(emptyWorkspaceView());
      return <><button onClick={() => model.newDefinition()}>New definition</button>{model.draft ? <TaskEditor model={model} /> : null}</>;
    }
    const user = userEvent.setup();
    const { container } = render(<Editor />, { wrapper: notificationWrapper(store) });
    await user.click(screen.getByRole("button", { name: "New definition" }));
    await user.type(screen.getByRole("textbox", { name: "Command line" }), "cargo build");
    const name = screen.getByRole("textbox", { name: "Name" });
    const directory = screen.getByRole("textbox", { name: "Working directory" });
    await user.type(name, " Build");
    await user.type(directory, "relative");
    await user.click(screen.getByRole("button", { name: "Create definition" }));
    await waitFor(() => expect(name.getAttribute("aria-invalid")).toBe("true"));
    expect(document.getElementById(name.getAttribute("aria-describedby")!)?.textContent).toContain("64 bytes");
    expect(container.querySelector(".task-editor > .inline-feedback")).toBeNull();
    await user.clear(name);
    await user.type(name, "Build");
    expect(name.getAttribute("aria-invalid")).toBe("false");
    await user.click(screen.getByRole("button", { name: "Create definition" }));
    await waitFor(() => expect(directory.getAttribute("aria-invalid")).toBe("true"));
    expect(document.getElementById(directory.getAttribute("aria-describedby")!)?.textContent).toBe("Use an absolute working directory.");
    await user.clear(directory);
    await user.type(directory, "/work/project");
    await user.click(screen.getByRole("button", { name: "Create definition" }));
    await waitFor(() => expect(container.querySelector(".task-editor > .inline-feedback")?.textContent).toBe("Use an absolute working directory."));
    expect(directory.getAttribute("aria-invalid")).toBe("false");
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(store.snapshot().entries).toHaveLength(0);
  });

  it("keeps validation in its editor without a notification or leaking into another draft", async () => {
    mockAvailableTasks();
    const store = new NotificationStore();
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()), { wrapper: notificationWrapper(store) });
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    act(() => result.current.model.newDefinition());
    await act(async () => { await result.current.model.save(); });
    expect(result.current.model.editor_error).toBe("Enter an executable.");
    expect(result.current.model.editor_validation_error).toMatchObject({ field: "program", field_value: "" });
    expect(store.snapshot().entries).toEqual([]);
    act(() => result.current.model.closeEditor());
    act(() => result.current.model.newDefinition());
    expect(result.current.model.editor_error).toBeNull();
  });

  it("does not deliver a late save failure into a replacement editor", async () => {
    mockAvailableTasks();
    let rejectSave!: (failure: Error) => void;
    api.saveTaskDefinition.mockImplementation(() => new Promise((_resolve, reject) => { rejectSave = reject; }));
    const store = new NotificationStore();
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()), { wrapper: notificationWrapper(store) });
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    act(() => result.current.model.newDefinition());
    act(() => result.current.model.edit(definition));
    let pending!: Promise<boolean>;
    await act(async () => { pending = result.current.model.save(); });
    act(() => result.current.model.closeEditor());
    act(() => result.current.model.newDefinition());
    await act(async () => { rejectSave(new Error("Could not save the original definition.")); await pending; });
    expect(result.current.model.editor_error).toBeNull();
    expect(store.snapshot().entries).toEqual([]);
  });

  it("notifies standalone command failures without injecting them into an open editor", async () => {
    mockAvailableTasks();
    api.taskRequest.mockImplementation(async (request: TaskRequest) => {
      if (request.type === "start_task") throw new Error("Task cannot start.");
      return { type: "task_list", tasks: [] };
    });
    const store = new NotificationStore();
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()), { wrapper: notificationWrapper(store) });
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    act(() => result.current.model.newDefinition());
    await act(async () => { await result.current.model.action(task, "start_task"); });
    expect(result.current.model.editor_error).toBeNull();
    expect(store.snapshot().entries).toEqual([expect.objectContaining({
      title: message("tasks.action_failed"), message: "Task cannot start.", toast_visible: true,
      actions: [{ label: message("tasks.view_task"), command_id: "task.open", args: { value: task.task_id } }],
    })]);
  });

  it("leaves workspace write failures to the workspace owner", async () => {
    mockAvailableTasks();
    const store = new NotificationStore();
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView(), new Error("Workspace is read-only.")), { wrapper: notificationWrapper(store) });
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    act(() => result.current.model.newDefinition());
    act(() => result.current.model.edit(definition));
    await act(async () => { await result.current.model.save(); });
    expect(result.current.model.editor_error).toBeNull();
    expect(api.saveTaskDefinition).not.toHaveBeenCalled();
    expect(store.snapshot().entries).toEqual([]);
  });

  it("opens changed definitions for contextual review without a failure notification", async () => {
    mockAvailableTasks();
    api.taskRequest.mockImplementation(async (request: TaskRequest) => request.type === "register_task"
      ? { type: "task_created", task: { ...task, task_id: request.task_id } }
      : { type: "task_list", tasks: [] });
    const store = new NotificationStore();
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()), { wrapper: notificationWrapper(store) });
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    const saved = { definition_id: "build", revision: "r2", definition: { ...definition, arguments: ["test"] } };
    await act(async () => { await result.current.model.run(saved); });
    expect(result.current.view.active_tab_key).toMatch(/^task:local:/);
    expect(api.taskRequest.mock.calls.some(([request]) => (request as TaskRequest).type === "start_task")).toBe(false);
    expect(store.snapshot().entries).toEqual([]);
  });

  it("keeps task and definition polling failures beside their lists with retry actions", async () => {
    api.loadTaskDefinitions.mockRejectedValue(new Error("Definitions are unavailable."));
    api.taskRequest.mockRejectedValue(new Error("Task daemon is unavailable."));
    const store = new NotificationStore();
    function TestView() {
      const { model } = useTestWorkspace(emptyWorkspaceView());
      return <TaskSidebar model={model} definitions={model.definitions} references={[]} />;
    }
    render(<TestView />, { wrapper: notificationWrapper(store) });
    await screen.findByText("Could not refresh task definitions: Definitions are unavailable.");
    await screen.findByText("Could not refresh tasks: Task daemon is unavailable.");
    expect(screen.getByRole("button", { name: "Retry definitions" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry tasks" })).toBeTruthy();
    expect(screen.queryByText("Loading tasks…")).toBeNull();
    expect(store.snapshot().entries).toEqual([]);
  });

  it("leaves editor-owned instance failures local and lets About own its action errors", async () => {
    mockAvailableTasks();
    api.taskRequest.mockImplementation(async (request: TaskRequest) => {
      if (request.type === "register_task") throw new Error("Could not create this instance.");
      return { type: "task_list", tasks: [] };
    });
    const store = new NotificationStore();
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()), { wrapper: notificationWrapper(store) });
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    act(() => result.current.model.newDefinition());
    const saved = { definition_id: "build", revision: "r1", definition };
    await act(async () => { expect(await result.current.model.runFromEditor(saved, "Another build", { kind: "global" })).toBe(false); });
    expect(result.current.model.editor_error).toBe("Could not create this instance.");
    await act(async () => {
      await expect(result.current.model.performComponentAction(async () => { throw new Error("About restart failed."); })).rejects.toThrow("About restart failed.");
    });
    expect(result.current.model.editor_error).toBe("Could not create this instance.");
    expect(store.snapshot().entries).toEqual([]);
  });

  it("keeps restart progress local and announces only the final outcome", async () => {
    mockAvailableTasks();
    let finish!: () => void;
    api.restartTaskDaemon.mockImplementation(() => new Promise<void>((resolve) => { finish = resolve; }));
    const store = new NotificationStore();
    const { result } = renderHook(() => useTestWorkspace(emptyWorkspaceView()), { wrapper: notificationWrapper(store) });
    await waitFor(() => expect(result.current.model.hasLoaded).toBe(true));
    let pending!: Promise<void>;
    act(() => { pending = result.current.model.restartDaemon(); });
    expect(result.current.model.daemonStatus).toBe("restarting");
    expect(store.snapshot().entries).toEqual([]);
    await act(async () => { finish(); await pending; });
    expect(result.current.model.daemonStatus).toBeNull();
    expect(store.snapshot().entries).toEqual([expect.objectContaining({ title: message("tasks.daemon_restarted_title"), severity: "success" })]);
  });
});

describe("task definition source identity", () => {
  it("canonicalizes saving and running a draft opened before its alias catalog loads", async () => {
    const catalog: TaskDefinitionCatalog = { scope: canonical, path: "/work/project/.ctl/tasks.json", definitions: [] };
    let currentCatalog = catalog;
    let finish!: (catalog: TaskDefinitionCatalog) => void;
    api.loadTaskDefinitions.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; })).mockImplementation(async () => currentCatalog);
    api.saveTaskDefinition.mockImplementation(async (_scope: TaskDefinitionScope, definition_id: string, _revision: string | null, definition: TaskDefinition) => {
      const saved = { definition_id, revision: "r1", definition };
      currentCatalog = { ...catalog, definitions: [saved] };
      return saved;
    });
    mockTaskRegistration();
    const { result } = renderHook(() => useTestWorkspace({ ...emptyWorkspaceView(), task_definition_scope: alias }));
    act(() => result.current.model.newDefinition());
    act(() => result.current.model.edit(definition));
    await act(async () => { await result.current.model.save(true); });
    expect(api.saveTaskDefinition).toHaveBeenCalledWith(canonical, expect.any(String), null, definition);
    expect(result.current.view.task_references).toHaveLength(1);
    expect(result.current.view.task_references[0].definition_scope).toEqual(canonical);
    expect(result.current.persisted.current.slice(-1)[0]?.task_references[0].definition_scope).toEqual(canonical);
    await act(async () => { finish(catalog); });
    expect(result.current.model.definitions).toHaveLength(1);
  });

  it("resolves a restored alias reference before reusing the default task identity", async () => {
    const saved = { definition_id: "build", revision: "r1", definition };
    api.loadTaskDefinitions.mockResolvedValue({ scope: canonical, path: "/work/project/.ctl/tasks.json", definitions: [saved] });
    mockTaskRegistration();
    const { result } = renderHook(() => useTestWorkspace({
      ...emptyWorkspaceView(), task_definition_scope: canonical,
      task_references: [{ host_id: "local", task_id: "existing-task", definition_id: saved.definition_id, definition_scope: alias, applied_revision: "r1", is_default: true }],
    }));
    await waitFor(() => expect(result.current.model.definitions).toEqual([saved]));
    await act(async () => { await result.current.model.run(saved); });
    expect(api.loadTaskDefinitions).toHaveBeenCalledWith(alias);
    expect(api.taskRequest).toHaveBeenCalledWith({ type: "register_task", task_id: "existing-task", definition });
    expect(result.current.view.task_references).toHaveLength(1);
    expect(result.current.view.task_references[0].definition_scope).toEqual(canonical);
    expect(result.current.persisted.current.slice(-1)[0]?.task_references[0].definition_scope).toEqual(canonical);
  });
});
