import { useCallback, useEffect, useRef, useState } from "react";
import type {
  SavedTaskDefinition,
  TaskDefinition,
  TaskDefinitionCatalog,
  TaskDefinitionScope,
} from "../../lib/types";
import { loadTaskDefinitions, removeTaskDefinition, saveTaskDefinition } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";

export const GLOBAL_DEFINITION_SCOPE: TaskDefinitionScope = { kind: "global" };

export function definitionScopeKey(scope: TaskDefinitionScope = GLOBAL_DEFINITION_SCOPE): string {
  return scope.kind === "global" ? "global" : `project:${scope.project_root}`;
}

export function definitionScopeLabel(scope: TaskDefinitionScope): string {
  return scope.kind === "global" ? "Global" : `Project · ${scope.project_root}`;
}

export function validProjectRoot(project_root: string): boolean {
  return /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(project_root) && !/[\x00-\x1f]/.test(project_root);
}

/** Catalogs are external view data; only scope selection and drafts belong in the workspace. */
export function useTaskDefinitions(scope: TaskDefinitionScope, ready: boolean) {
  const [catalogs, setCatalogs] = useState<Record<string, TaskDefinitionCatalog>>({});
  const [errors, setErrors] = useState<Record<string, string | null>>({});
  const [loading, setLoading] = useState<Record<string, boolean>>({});
  const epochs = useRef<Record<string, number>>({});
  const requestGeneration = useRef(0);
  const scopeGenerations = useRef<Record<string, number>>({});
  const canonicalScopes = useRef<Record<string, TaskDefinitionScope>>({});
  const mounted = useRef(true);
  const scopeRef = useRef(scope);
  scopeRef.current = scope;
  const key = definitionScopeKey(scope);

  const resolveScope = useCallback((source: TaskDefinitionScope = GLOBAL_DEFINITION_SCOPE) =>
    canonicalScopes.current[definitionScopeKey(source)] ?? source, []);
  const advanceGeneration = useCallback((source_key: string) => {
    const generation = ++requestGeneration.current;
    scopeGenerations.current[source_key] = generation;
    return generation;
  }, []);

  const load = useCallback(async (source: TaskDefinitionScope): Promise<TaskDefinitionCatalog> => {
    const source_key = definitionScopeKey(source);
    const generation = advanceGeneration(source_key);
    scopeGenerations.current[definitionScopeKey(resolveScope(source))] = generation;
    const epoch = (epochs.current[source_key] ?? 0) + 1;
    epochs.current[source_key] = epoch;
    setLoading((current) => ({ ...current, [source_key]: true }));
    try {
      const catalog = await loadTaskDefinitions(source);
      const canonical_key = definitionScopeKey(catalog.scope);
      canonicalScopes.current[source_key] = catalog.scope;
      canonicalScopes.current[canonical_key] = catalog.scope;
      if (mounted.current && epochs.current[source_key] === epoch) {
        // Different submitted paths can resolve to one store. Fence that shared identity too.
        if ((scopeGenerations.current[canonical_key] ?? 0) <= generation) {
          scopeGenerations.current[canonical_key] = generation;
          setCatalogs((current) => ({ ...current, [source_key]: catalog, [canonical_key]: catalog }));
        }
        setErrors((current) => ({ ...current, [source_key]: null }));
      }
      return catalog;
    } catch (failure) {
      if (mounted.current && epochs.current[source_key] === epoch)
        setErrors((current) => ({ ...current, [source_key]: errorMessage(failure) }));
      throw failure;
    } finally {
      if (mounted.current && epochs.current[source_key] === epoch)
        setLoading((current) => ({ ...current, [source_key]: false }));
    }
  }, [advanceGeneration, resolveScope]);

  const ensureScope = useCallback(async (source: TaskDefinitionScope = GLOBAL_DEFINITION_SCOPE) => {
    if (source.kind === "global") return source;
    return canonicalScopes.current[definitionScopeKey(source)] ?? (await load(source)).scope;
  }, [load]);

  const refresh = useCallback(async () => {
    try { await load(scopeRef.current); } catch { /* The catalog retains its own visible error. */ }
  }, [load]);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    if (!ready) return;
    void refresh();
    const onFocus = () => void refresh();
    const onVisibility = () => { if (document.visibilityState === "visible") void refresh(); };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [key, ready, refresh]);

  const save = async (
    source: TaskDefinitionScope,
    definition_id: string,
    expected_revision: string | null,
    definition: TaskDefinition,
  ): Promise<SavedTaskDefinition> => {
    source = await ensureScope(source);
    const source_key = definitionScopeKey(source);
    advanceGeneration(source_key);
    epochs.current[source_key] = (epochs.current[source_key] ?? 0) + 1;
    let saved: SavedTaskDefinition;
    try {
      saved = await saveTaskDefinition(source, definition_id, expected_revision, definition);
    } finally {
      advanceGeneration(source_key);
      epochs.current[source_key] += 1;
      if (mounted.current) setLoading((current) => ({ ...current, [source_key]: false }));
    }
    if (mounted.current) {
      setErrors((current) => ({ ...current, [source_key]: null }));
      setCatalogs((current) => ({
        ...current,
        [source_key]: {
          scope: source,
          path: current[source_key]?.path ?? "",
          definitions: [
            ...(current[source_key]?.definitions ?? []).filter((item) => item.definition_id !== definition_id),
            saved,
          ],
        },
      }));
    }
    return saved;
  };

  const remove = async (source: TaskDefinitionScope, definition_id: string, expected_revision: string) => {
    source = await ensureScope(source);
    const source_key = definitionScopeKey(source);
    advanceGeneration(source_key);
    epochs.current[source_key] = (epochs.current[source_key] ?? 0) + 1;
    try {
      await removeTaskDefinition(source, definition_id, expected_revision);
    } finally {
      advanceGeneration(source_key);
      epochs.current[source_key] += 1;
      if (mounted.current) setLoading((current) => ({ ...current, [source_key]: false }));
    }
    if (mounted.current) {
      setErrors((current) => ({ ...current, [source_key]: null }));
      setCatalogs((current) => {
        const catalog = current[source_key];
        return catalog ? { ...current, [source_key]: {
          ...catalog,
          definitions: catalog.definitions.filter((item) => item.definition_id !== definition_id),
        } } : current;
      });
    }
  };

  const currentCatalog = catalogs[definitionScopeKey(resolveScope(scope))] ?? catalogs[key];
  return {
    scope: currentCatalog?.scope ?? scope,
    definitions: currentCatalog?.definitions ?? [],
    path: currentCatalog?.path ?? null,
    loading: loading[key] ?? false,
    has_loaded: !!currentCatalog,
    error: errors[key] ?? null,
    get: (source: TaskDefinitionScope) => {
      return catalogs[definitionScopeKey(resolveScope(source))] ?? catalogs[definitionScopeKey(source)];
    },
    resolveScope,
    ensureScope,
    load,
    refresh,
    save,
    remove,
  };
}

export type TaskDefinitions = ReturnType<typeof useTaskDefinitions>;
