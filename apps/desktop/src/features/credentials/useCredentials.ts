import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/errors";
import { forgetIdentityPassphrase, forgetSavedCredential, listIdentityFiles, listSavedCredentials, saveIdentityPassphrase } from "../../lib/tauri";
import type { CredentialRecord, CredentialTarget, CredentialsSnapshot, IdentityFile, IdentitySnapshot } from "../../lib/types";
import { canForgetIdentity, canSaveIdentity, identityMutationError, identityName } from "./identityFiles";

interface IdentityDialog {
  file: IdentityFile;
  action: "save" | "forget" | "saving" | "forgetting";
}

/** Inventories contain metadata only. A submitted passphrase is sent directly to native verification. */
export function useCredentials(visible: boolean, targets: CredentialTarget[]) {
  const [snapshot, setSnapshot] = useState<CredentialsSnapshot | null>(null);
  const [identities, setIdentities] = useState<IdentitySnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [identity_error, setIdentityError] = useState<string | null>(null);
  const [pending, setPending] = useState<CredentialRecord | null>(null);
  const [identity_dialog, setIdentityDialog] = useState<IdentityDialog | null>(null);
  const [busy_id, setBusyId] = useState<string | null>(null);
  const [action_error, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const mounted = useRef(false);
  const generation = useRef(0);
  const lock = useRef<"dialog" | "mutation" | null>(null);
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
    const [credentials_result, identities_result] = await Promise.allSettled([
      listSavedCredentials(targets_ref.current.filter(({ target }) => !target.unavailable)),
      listIdentityFiles(targets_ref.current),
    ]);
    if (!mounted.current || request_generation !== generation.current) return;
    if (credentials_result.status === "fulfilled") {
      setSnapshot(credentials_result.value);
      setError(null);
    } else setError(errorMessage(credentials_result.reason));
    if (identities_result.status === "fulfilled") {
      setIdentities(identities_result.value);
      setIdentityError(null);
    } else setIdentityError(errorMessage(identities_result.reason));
    setLoading(false);
  }, []);

  useEffect(() => {
    if (visible) void refresh();
    else {
      generation.current += 1;
      setLoading(false);
      setPending(null);
      setIdentityDialog(null);
      if (lock.current === "dialog") lock.current = null;
    }
  }, [visible, target_key, refresh]);

  const openDialog = useCallback(() => {
    if (!visible_ref.current || lock.current !== null) return false;
    lock.current = "dialog";
    setActionError(null);
    setNotice(null);
    return true;
  }, []);

  const requestForget = useCallback((credential: CredentialRecord) => {
    if (credential.storage !== "keychain" || credential.action !== "forget" || !openDialog()) return;
    setPending(credential);
  }, [openDialog]);

  const cancelDialog = useCallback(() => {
    if (lock.current === "mutation") return;
    lock.current = null;
    setPending(null);
    setIdentityDialog(null);
  }, []);

  const beginMutation = useCallback((id: string) => {
    if (!visible_ref.current || lock.current !== "dialog") return false;
    lock.current = "mutation";
    generation.current += 1;
    setLoading(false);
    setBusyId(id);
    return true;
  }, []);

  const finishMutation = useCallback(() => {
    lock.current = null;
    if (mounted.current) {
      setBusyId(null);
      setIdentityDialog(null);
    }
  }, []);

  const confirmForget = useCallback(async () => {
    if (!pending || !beginMutation(pending.credential_id)) return;
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
    } finally { finishMutation(); }
  }, [pending, beginMutation, finishMutation, refresh]);

  const requestIdentitySave = useCallback((file: IdentityFile) => {
    if (!canSaveIdentity(file, identities?.keychain_available === true) || identity_error || !openDialog()) return;
    setIdentityDialog({ file, action: "save" });
  }, [identities, identity_error, openDialog]);

  const requestIdentityForget = useCallback((file: IdentityFile) => {
    if (!canForgetIdentity(file, identities?.keychain_available === true) || identity_error || !openDialog()) return;
    setIdentityDialog({ file, action: "forget" });
  }, [identities, identity_error, openDialog]);

  const confirmIdentitySave = useCallback(async (passphrase: string) => {
    const file = identity_dialog?.action === "save" ? identity_dialog.file : null;
    if (!file?.file_version || !beginMutation(file.identity_id)) return;
    // Replace the input with progress immediately; no password lives in React state.
    setIdentityDialog({ file, action: "saving" });
    try {
      const operation = saveIdentityPassphrase({ path: file.path, file_version: file.file_version, passphrase });
      passphrase = "";
      await operation;
      if (!mounted.current) return;
      setIdentities((previous) => previous ? {
        ...previous,
        identity_files: previous.identity_files.map((row) => row.identity_id === file.identity_id && row.file_version === file.file_version
          ? { ...row, passphrase_state: "saved" } : row),
      } : null);
      setNotice(`Saved the passphrase for ${identityName(file)} in Keychain.`);
      await refresh();
    } catch (failure) {
      if (mounted.current) setActionError(identityMutationError(failure));
    } finally { finishMutation(); }
  }, [identity_dialog, beginMutation, finishMutation, refresh]);

  const confirmIdentityForget = useCallback(async () => {
    const file = identity_dialog?.action === "forget" ? identity_dialog.file : null;
    if (!file || !beginMutation(file.identity_id)) return;
    setIdentityDialog({ file, action: "forgetting" });
    try {
      await forgetIdentityPassphrase(file.identity_id);
      if (!mounted.current) return;
      setIdentities((previous) => previous ? {
        ...previous,
        identity_files: previous.identity_files.map((row) => row.identity_id === file.identity_id
          ? { ...row, passphrase_state: row.encrypted === false ? "not_required" : row.encrypted === true ? "not_saved" : "unknown" } : row),
      } : null);
      setNotice(`Forgot the passphrase for ${identityName(file)}. The identity file is unchanged.`);
      await refresh();
    } catch (failure) {
      if (mounted.current) setActionError(identityMutationError(failure, "forget"));
    } finally { finishMutation(); }
  }, [identity_dialog, beginMutation, finishMutation, refresh]);

  return {
    snapshot, identities, loading, error, identity_error, pending, identity_dialog, busy_id, action_error, notice,
    refresh, requestForget, cancelForget: cancelDialog, confirmForget, cancelDialog,
    requestIdentitySave, requestIdentityForget, confirmIdentitySave, confirmIdentityForget,
  };
}
