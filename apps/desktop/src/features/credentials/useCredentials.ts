import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/errors";
import { forgetSavedCredential, listSavedCredentials } from "../../lib/tauri";
import type { CredentialRecord, CredentialTarget, CredentialsSnapshot } from "../../lib/types";

/** This inventory never owns an SSH connection or reads a credential's value. */
export function useCredentials(visible: boolean, targets: CredentialTarget[]) {
  const [snapshot, setSnapshot] = useState<CredentialsSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<CredentialRecord | null>(null);
  const [busy_id, setBusyId] = useState<string | null>(null);
  const [action_error, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const mounted = useRef(false);
  const generation = useRef(0);
  const mutation_pending = useRef(false);
  const visible_ref = useRef(visible);
  const targets_ref = useRef(targets);
  visible_ref.current = visible;
  targets_ref.current = targets;
  const target_key = JSON.stringify(targets);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current += 1; };
  }, []);

  const refresh = useCallback(async () => {
    if (!visible_ref.current) return;
    const request_generation = ++generation.current;
    setLoading(true);
    try {
      const next = await listSavedCredentials(targets_ref.current);
      if (!mounted.current || request_generation !== generation.current) return;
      setSnapshot(next);
      setError(null);
    } catch (failure) {
      if (mounted.current && request_generation === generation.current) setError(errorMessage(failure));
    } finally {
      if (mounted.current && request_generation === generation.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (visible) void refresh();
    else {
      generation.current += 1;
      setLoading(false);
      setPending(null);
    }
  }, [visible, target_key, refresh]);

  const requestForget = useCallback((credential: CredentialRecord) => {
    if (!visible_ref.current || mutation_pending.current || credential.storage !== "keychain" || credential.action !== "forget") return;
    setActionError(null);
    setNotice(null);
    setPending(credential);
  }, []);

  const cancelForget = useCallback(() => setPending(null), []);

  const confirmForget = useCallback(async () => {
    if (!pending || mutation_pending.current || !visible_ref.current) return;
    mutation_pending.current = true;
    // An observation started before deletion must not put the removed row back.
    generation.current += 1;
    setLoading(false);
    setBusyId(pending.credential_id);
    setPending(null);
    try {
      await forgetSavedCredential(pending.credential_id);
      if (!mounted.current) return;
      setSnapshot((previous) => previous ? {
        ...previous,
        credentials: previous.credentials.filter((row) => row.credential_id !== pending.credential_id),
      } : null);
      setNotice(`Forgot ${pending.name}. Active connections are unchanged.`);
      await refresh();
    } catch (failure) {
      if (mounted.current) setActionError(`Could not forget ${pending.name}: ${errorMessage(failure)}`);
    } finally {
      mutation_pending.current = false;
      if (mounted.current) setBusyId(null);
    }
  }, [pending, refresh]);

  return { snapshot, loading, error, pending, busy_id, action_error, notice, refresh, requestForget, cancelForget, confirmForget };
}
