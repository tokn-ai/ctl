// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useTaskDefinitions } from "./useTaskDefinitions";
import type { SavedTaskDefinition, TaskDefinitionCatalog } from "../../lib/types";

const api = vi.hoisted(() => ({ loadTaskDefinitions: vi.fn(), saveTaskDefinition: vi.fn(), removeTaskDefinition: vi.fn() }));
vi.mock("../../lib/tauri", () => api);
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const scope = { kind: "global" as const };
const saved: SavedTaskDefinition = { definition_id: "build", revision: "r1", definition: { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" } };
const catalog: TaskDefinitionCatalog = { scope, path: "/test/definitions.json", definitions: [saved] };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((finish) => { resolve = finish; });
  return { promise, resolve };
}

describe("shared task definition catalogs", () => {
  it.each(["save", "remove"] as const)("clears pending-load state after a rejected %s", async (mutation) => {
    let finish!: (catalog: TaskDefinitionCatalog) => void;
    api.loadTaskDefinitions.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const failure = { code: "definition_conflict", message: "The definition changed." };
    api.saveTaskDefinition.mockRejectedValue(failure);
    api.removeTaskDefinition.mockRejectedValue(failure);
    const { result } = renderHook(() => useTaskDefinitions(scope, true));
    await waitFor(() => expect(result.current.loading).toBe(true));
    await act(async () => {
      const operation = mutation === "save"
        ? result.current.save(scope, saved.definition_id, saved.revision, saved.definition)
        : result.current.remove(scope, saved.definition_id, saved.revision);
      await expect(operation).rejects.toEqual(failure);
    });
    expect(result.current.loading).toBe(false);
    await act(async () => { finish(catalog); });
    expect(result.current.loading).toBe(false);
    expect(result.current.definitions).toEqual([]);
  });

  it("does not let an older focus refresh replace a successful save", async () => {
    api.loadTaskDefinitions.mockResolvedValueOnce(catalog);
    const { result } = renderHook(() => useTaskDefinitions(scope, true));
    await waitFor(() => expect(result.current.definitions).toEqual([saved]));
    let finish!: (catalog: TaskDefinitionCatalog) => void;
    api.loadTaskDefinitions.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    act(() => { window.dispatchEvent(new Event("focus")); });
    const newer = { ...saved, revision: "r2", definition: { ...saved.definition, name: "Changed" } };
    api.saveTaskDefinition.mockResolvedValue(newer);
    await act(async () => { await result.current.save(scope, saved.definition_id, "r1", newer.definition); });
    await act(async () => { finish(catalog); });
    expect(result.current.definitions).toEqual([newer]);
  });

  it("retains canonical project identity returned by the native catalog", async () => {
    const submitted = { kind: "project" as const, project_root: "/work/link/../project" };
    const canonical = { kind: "project" as const, project_root: "/work/project" };
    api.loadTaskDefinitions.mockResolvedValue({ ...catalog, scope: canonical });
    api.saveTaskDefinition.mockResolvedValue({ ...saved, revision: "r2" });
    const { result } = renderHook(() => useTaskDefinitions(submitted, true));
    await waitFor(() => expect(result.current.scope).toEqual(canonical));
    await act(async () => { await result.current.save(submitted, "build", "r1", saved.definition); });
    expect(api.saveTaskDefinition).toHaveBeenCalledWith(canonical, "build", "r1", saved.definition);
    expect(result.current.definitions[0].revision).toBe("r2");
    expect(result.current.get(submitted)?.definitions[0].revision).toBe("r2");
  });

  it.each(["save", "remove"] as const)("fences an older alias refresh after a canonical %s", async (mutation) => {
    const alias = { kind: "project" as const, project_root: "/work/link" };
    const canonical = { kind: "project" as const, project_root: "/work/project" };
    const original = { ...catalog, scope: canonical };
    api.loadTaskDefinitions.mockResolvedValueOnce(original);
    const { result } = renderHook(() => useTaskDefinitions(alias, true));
    await waitFor(() => expect(result.current.definitions).toEqual([saved]));
    const pending = deferred<TaskDefinitionCatalog>();
    api.loadTaskDefinitions.mockReturnValueOnce(pending.promise);
    let refresh!: Promise<TaskDefinitionCatalog>;
    act(() => { refresh = result.current.load(alias); });
    const newer = { ...saved, revision: "r2" };
    api.saveTaskDefinition.mockResolvedValue(newer);
    api.removeTaskDefinition.mockResolvedValue(undefined);
    await act(async () => {
      if (mutation === "save") await result.current.save(canonical, saved.definition_id, "r1", saved.definition);
      else await result.current.remove(canonical, saved.definition_id, "r1");
    });
    await act(async () => { pending.resolve(original); await refresh; });
    const expected = mutation === "save" ? [newer] : [];
    expect(result.current.get(alias)?.definitions).toEqual(expected);
    expect(result.current.get(canonical)?.definitions).toEqual(expected);
    expect(result.current.loading).toBe(false);
  });

  it("orders previously unknown aliases by request generation", async () => {
    const alias = { kind: "project" as const, project_root: "/work/link" };
    const other = { kind: "project" as const, project_root: "/work/other-link" };
    const canonical = { kind: "project" as const, project_root: "/work/project" };
    const older = deferred<TaskDefinitionCatalog>();
    const newer = deferred<TaskDefinitionCatalog>();
    api.loadTaskDefinitions.mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
    const { result } = renderHook(() => useTaskDefinitions(alias, true));
    let refresh!: Promise<TaskDefinitionCatalog>;
    act(() => { refresh = result.current.load(other); });
    const changed = { ...saved, revision: "r2" };
    await act(async () => { newer.resolve({ ...catalog, scope: canonical, definitions: [changed] }); await refresh; });
    await act(async () => { older.resolve({ ...catalog, scope: canonical }); });
    expect(result.current.get(alias)?.definitions).toEqual([changed]);
    expect(result.current.get(other)?.definitions).toEqual([changed]);
    expect(result.current.get(canonical)?.definitions).toEqual([changed]);
  });
});
