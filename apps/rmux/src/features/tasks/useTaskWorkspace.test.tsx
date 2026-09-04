// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useTaskWorkspace } from "./useTaskWorkspace";
import { TaskSidebar } from "../../components/tasks/TaskSidebar";
import { emptyWorkspaceView } from "../workspace/workspaceModel";
import type { useWorkspace } from "../workspace/useWorkspace";
const api = vi.hoisted(() => ({ taskRequest: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ taskRequest: api.taskRequest, inspectKnownSessions: vi.fn() }));
afterEach(() => { cleanup(); vi.useRealTimers(); vi.resetAllMocks(); });

describe("task background refresh", () => {
  it("keeps the empty state and refresh control stable while a poll is pending", async () => {
    vi.useFakeTimers();
    const view = emptyWorkspaceView();
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
