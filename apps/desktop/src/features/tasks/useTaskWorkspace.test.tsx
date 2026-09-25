// @vitest-environment jsdom
import { act, cleanup, render, renderHook, screen, waitFor } from "@testing-library/react";
import { useRef, useState, type SetStateAction } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useTaskWorkspace } from "./useTaskWorkspace";
import { TaskSidebar } from "../../components/tasks/TaskSidebar";
import { emptyWorkspaceView, type WorkspaceView } from "../workspace/workspaceModel";
import type { useWorkspace } from "../workspace/useWorkspace";
import type { TaskDefinition, TaskDefinitionCatalog, TaskDefinitionScope, TaskRequest } from "../../lib/types";
const api = vi.hoisted(() => ({ taskRequest: vi.fn(), loadTaskDefinitions: vi.fn(), saveTaskDefinition: vi.fn(), removeTaskDefinition: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ ...api, inspectKnownSessions: vi.fn() }));
afterEach(() => { cleanup(); vi.useRealTimers(); vi.resetAllMocks(); });

function useTestWorkspace(initial: WorkspaceView) {
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
    persist: async () => { persisted.current.push(viewRef.current); },
    setActiveTabKey: (key: string | null) => update("active_tab_key", key),
  } as unknown as ReturnType<typeof useWorkspace>;
  return { model: useTaskWorkspace(workspace, async () => {}, async () => {}), view, persisted };
}

const alias: TaskDefinitionScope = { kind: "project", project_root: "/work/link" };
const canonical: TaskDefinitionScope = { kind: "project", project_root: "/work/project" };
const definition: TaskDefinition = { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" };

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
