import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/errors";
import { getComponentVersions, preflightComponentAction, executeComponentAction } from "../../lib/tauri";
import type { ComponentVersionsSnapshot, ComponentActionPreflight, ComponentActionResult } from "../../lib/types";

/** Observations never own connections. Closing About must not stop any work. */
export function useComponentVersions(visible: boolean, on_restarted: (preflight: ComponentActionPreflight) => void, execute_action?: (preflight: ComponentActionPreflight) => Promise<ComponentActionResult>) {
  const [snapshot, setSnapshot] = useState<ComponentVersionsSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [checked_at, setCheckedAt] = useState<number | null>(null);
  const [preflight, setPreflight] = useState<ComponentActionPreflight | null>(null);
  const [busy_id, setBusyId] = useState<string | null>(null);
  const [restarting, setRestarting] = useState(false);
  const [action_error, setActionError] = useState<{ component_id: string; message: string } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const mounted = useRef(false);
  const visible_ref = useRef(visible);
  visible_ref.current = visible;
  const generation = useRef(0);
  const action_pending = useRef(false);
  const action_generation = useRef(0);
  const confirmed_token = useRef<string | null>(null);
  const restarted = useRef(on_restarted);
  restarted.current = on_restarted;

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current += 1; };
  }, []);

  const refresh = useCallback(async (host_id?: string) => {
    const request_generation = ++generation.current;
    setLoading(true);
    try {
      const next = await getComponentVersions(host_id);
      if (!mounted.current || request_generation !== generation.current) return;
      setSnapshot(next);
      setCheckedAt(Date.now());
      setError(null);
    } catch (failure) {
      if (mounted.current && request_generation === generation.current) setError(errorMessage(failure));
    } finally {
      if (mounted.current && request_generation === generation.current) setLoading(false);
    }
  }, []);

  useEffect(() => { if (visible) void refresh(); }, [visible, refresh]);

  useEffect(() => {
    if (visible || confirmed_token.current !== null) return;
    action_generation.current += 1;
    action_pending.current = false;
    setBusyId(null);
    setPreflight(null);
  }, [visible]);

  const requestRestart = useCallback(async (component_id: string) => {
    if (action_pending.current) return;
    action_pending.current = true;
    const request_generation = ++action_generation.current;
    setBusyId(component_id);
    setActionError(null);
    setNotice(null);
    try {
      const next = await preflightComponentAction(component_id);
      if (request_generation !== action_generation.current) return;
      if (mounted.current && visible_ref.current) setPreflight(next);
      else {
        action_pending.current = false;
        if (mounted.current) setBusyId(null);
      }
    } catch (failure) {
      if (request_generation !== action_generation.current) return;
      if (mounted.current) setActionError({ component_id, message: errorMessage(failure) });
      action_pending.current = false;
      if (mounted.current) setBusyId(null);
    }
  }, []);

  const cancelRestart = useCallback(() => {
    if (confirmed_token.current !== null) return;
    action_pending.current = false;
    setBusyId(null);
    setPreflight(null);
  }, []);

  const confirmRestart = useCallback(async () => {
    if (!preflight || confirmed_token.current !== null) return;
    confirmed_token.current = preflight.action_token;
    setPreflight(null);
    setRestarting(true);
    try {
      const result = await (execute_action ? execute_action(preflight) : executeComponentAction(preflight.action_token));
      if (mounted.current) setNotice(result.detail ?? `${preflight.label} ${preflight.action === "reconnect" ? "reconnected" : "restarted"}.`);
    } catch (failure) {
      if (mounted.current) setActionError({ component_id: preflight.component_id, message: errorMessage(failure) });
    } finally {
      // A restart may have taken effect even if its final health check failed.
      restarted.current(preflight);
      if (mounted.current) {
        setBusyId(null);
        setRestarting(false);
        await refresh();
      }
      confirmed_token.current = null;
      action_pending.current = false;
    }
  }, [preflight, refresh, execute_action]);

  return { snapshot, loading, error, checked_at, preflight, busy_id, restarting, action_error, notice, refresh, requestRestart, cancelRestart, confirmRestart };
}
