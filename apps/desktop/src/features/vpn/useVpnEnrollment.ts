import { useCallback, useEffect, useRef, useState } from "react";
import { errorCode, errorMessage } from "../../lib/errors";
import { beginVpnEnrollment, cancelVpnEnrollment, openVpnSignIn, vpnEnrollmentStatus } from "../../lib/tauri";
import type { VpnEnrollmentInput, VpnEnrollmentSnapshot } from "../../lib/types";
import { vpnNeedsSignIn } from "./status";

export const VPN_ENROLLMENT_INTERVAL_MS = 1_000;

interface Options {
  on_connection_id?(connection_id: string | null): void;
  on_save?(enrollment_id: string): Promise<boolean>;
  on_close(): void;
}

/** Own a temporary native enrollment until it is adopted or explicitly discarded. */
export function useVpnEnrollment(options: Options) {
  const [snapshot, setSnapshot] = useState<VpnEnrollmentSnapshot | null>(null);
  const [started, setStarted] = useState(false);
  const [starting, setStarting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [cleanup_pending, setCleanupPending] = useState(false);
  const [opening_browser, setOpeningBrowser] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [browser_error, setBrowserError] = useState<string | null>(null);
  const mounted = useRef(false);
  const callbacks = useRef(options);
  callbacks.current = options;
  const current = useRef<VpnEnrollmentSnapshot | null>(null);
  const begin_pending = useRef<Promise<void> | null>(null);
  const cancel_pending = useRef<Promise<void> | null>(null);
  const discard_pending = useRef<Promise<void> | null>(null);
  const status_pending = useRef<Promise<void> | null>(null);
  const cancel_requested = useRef(false);
  const adopted = useRef(false);
  const saving = useRef(false);
  const opening = useRef(false);
  const opened_urls = useRef(new Set<string>());

  const discard = useCallback(async () => {
    if (adopted.current) return;
    if (discard_pending.current) return discard_pending.current;
    const enrollment = current.current;
    if (!enrollment) return;
    const pending = (async () => {
      await cancelVpnEnrollment(enrollment.enrollment_id);
      if (current.current?.enrollment_id === enrollment.enrollment_id) current.current = null;
    })();
    discard_pending.current = pending;
    try { await pending; }
    finally { if (discard_pending.current === pending) discard_pending.current = null; }
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      cancel_requested.current = true;
      if (!saving.current) {
        void (async () => {
          await begin_pending.current;
          await discard();
        })().catch(() => undefined);
      }
    };
  }, [discard]);

  const publish = useCallback((next: VpnEnrollmentSnapshot) => {
    current.current = next;
    if (!mounted.current || cancel_requested.current) return;
    setSnapshot(next);
    setError(null);
    setCleanupPending(false);
    if (next.status.state === "connected") setBrowserError(null);
    callbacks.current.on_connection_id?.(next.connection_id);
  }, []);

  const refresh = useCallback(async () => {
    const enrollment = current.current;
    if (!enrollment || cancel_requested.current || adopted.current) return;
    if (status_pending.current) return status_pending.current;
    const pending = (async () => {
      try {
        const next = await vpnEnrollmentStatus(enrollment.enrollment_id);
        if (mounted.current && !cancel_requested.current && current.current?.enrollment_id === enrollment.enrollment_id) publish(next);
      } catch (failure) {
        if (mounted.current && !cancel_requested.current && current.current?.enrollment_id === enrollment.enrollment_id) setError(errorMessage(failure));
      }
    })();
    status_pending.current = pending;
    try { await pending; }
    finally { if (status_pending.current === pending) status_pending.current = null; }
  }, [publish]);

  useEffect(() => {
    if (!snapshot || cancelling || snapshot.error) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      await refresh();
      if (!disposed) timer = setTimeout(() => void tick(), VPN_ENROLLMENT_INTERVAL_MS);
    };
    timer = setTimeout(() => void tick(), VPN_ENROLLMENT_INTERVAL_MS);
    const focus = () => void refresh();
    window.addEventListener("focus", focus);
    return () => { disposed = true; clearTimeout(timer); window.removeEventListener("focus", focus); };
  }, [snapshot?.enrollment_id, Boolean(snapshot?.error), cancelling, refresh]);

  const openBrowser = useCallback(async () => {
    const enrollment = current.current;
    if (!enrollment || enrollment.error || opening.current || cancel_requested.current || adopted.current || !vpnNeedsSignIn(enrollment.status)) return;
    opening.current = true;
    setOpeningBrowser(true);
    setBrowserError(null);
    try {
      await openVpnSignIn(enrollment.connection_id);
      if (mounted.current && !cancel_requested.current) void refresh();
    } catch (failure) {
      if (mounted.current && !cancel_requested.current && vpnNeedsSignIn(current.current?.status)) setBrowserError(errorMessage(failure));
    } finally {
      opening.current = false;
      if (mounted.current) setOpeningBrowser(false);
    }
  }, [refresh]);

  useEffect(() => {
    const url = snapshot?.status.auth_url;
    if (!url || snapshot?.error || !vpnNeedsSignIn(snapshot?.status) || cancel_requested.current || opened_urls.current.has(url)) return;
    opened_urls.current.add(url);
    void openBrowser();
  }, [snapshot, openBrowser]);

  const begin = useCallback(async (input: VpnEnrollmentInput) => {
    if (begin_pending.current || saving.current || cancel_pending.current || adopted.current) return;
    cancel_requested.current = false;
    setStarted(true);
    setStarting(true);
    setError(null);
    setCleanupPending(false);
    setBrowserError(null);
    const pending = (async () => {
      try {
        // Retrying first discards the failed draft, never leaving two enrollments behind.
        await discard();
        if (cancel_requested.current || !mounted.current) return;
        opened_urls.current.clear();
        const next = await beginVpnEnrollment(input);
        publish(next);
      } catch (failure) {
        if (mounted.current && !cancel_requested.current) setError(errorMessage(failure));
      } finally {
        if (mounted.current) setStarting(false);
      }
    })();
    begin_pending.current = pending;
    await pending;
    if (begin_pending.current === pending) begin_pending.current = null;
  }, [discard, publish]);

  const cancel = useCallback(async () => {
    if (saving.current || cancel_pending.current) return;
    cancel_requested.current = true;
    setCancelling(true);
    const pending = (async () => {
      try {
        await begin_pending.current;
        await discard();
        if (mounted.current) {
          setCleanupPending(false);
          callbacks.current.on_connection_id?.(null);
          callbacks.current.on_close();
        }
      } catch (failure) {
        if (mounted.current) {
          const pending_cleanup = errorCode(failure) === "vpn_cleanup_pending";
          setCleanupPending(pending_cleanup);
          setError(pending_cleanup ? errorMessage(failure) : `Could not cancel this sign-in: ${errorMessage(failure)}`);
        }
      } finally {
        if (mounted.current) setCancelling(false);
      }
    })();
    cancel_pending.current = pending;
    await pending;
    if (cancel_pending.current === pending) cancel_pending.current = null;
  }, [discard]);

  const ready = snapshot?.status.state === "connected" && !snapshot.status.status_unavailable && snapshot.status.running && Boolean(snapshot.status.endpoint) && !snapshot.error && !error;
  const save = useCallback(async () => {
    if (!ready || !current.current || saving.current || cancel_requested.current || !callbacks.current.on_save) return false;
    saving.current = true;
    try {
      const saved = await callbacks.current.on_save(current.current.enrollment_id);
      if (saved) adopted.current = true;
      return saved;
    } finally {
      saving.current = false;
      if (!mounted.current && !adopted.current) void discard().catch(() => undefined);
    }
  }, [discard, ready]);

  return { snapshot, started, starting, cancelling, cleanup_pending, opening_browser, error, browser_error, ready, begin, cancel, openBrowser, refresh, save };
}
