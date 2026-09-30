import { useCallback, useEffect, useRef, useState } from "react";
import type { Channel } from "@tauri-apps/api/core";
import { decodeBase64, encodeBase64, sequenceAtLeast } from "../../lib/bytes";
import {
  acknowledgeAttachmentEvent,
  acquireAttachmentLease,
  detachAttachment,
  openAttachment,
  releaseAttachmentLease,
  resizeAttachment,
  sendInput,
  sessionCache,
} from "../../lib/tauri";
import { errorCode, errorMessage } from "../../lib/errors";
import type {
  AttachmentEvent,
  AttachmentViewState,
  ConnectionPhase,
  LeaseKind,
  SessionSummary,
  ShellStateSummary,
  TerminalSize,
} from "../../lib/types";
import type { ProposedDimensions } from "../terminal/TerminalPresenter";
import type { AttachmentRenderer } from "../terminal/XtermRenderer";
import { sameSession, sessionKey, targetKey } from "../targets/targets";
import { sameSshEndpoint } from "../workspace/remoteRecovery";
import {
  ATTACHMENT_RECOVERY_STABILITY_MS,
  AttachmentRecoveryBackoff,
  canAutomaticallyRecoverAttachment,
  interruptedAttachmentState,
  reconnectSequenceAfterError,
} from "./attachmentRecovery";
import { registerAttachmentControl } from "./componentActions";
import { initialAttachmentState, transitionAttachment, type ConnectionIntent } from "./attachmentState";
import { ConnectionIntentQueue } from "./ConnectionIntentQueue";
import { InputPump } from "./InputPump";
import {
  LayoutLeasePump,
  shouldStopResizeAfterLeaseStatus,
} from "./LayoutLeasePump";
import { LatestTaskQueue } from "./LatestTaskQueue";
import { useManualReconnect } from "./ManualReconnect";
import { ResizeCoordinator } from "./ResizeCoordinator";
import { ResizePump } from "./ResizePump";

const INITIAL_STATE = initialAttachmentState();

function terminalSize(columns: number, rows: number): TerminalSize {
  return {
    columns,
    rows,
    pixel_width: null,
    pixel_height: null,
  };
}

function matchesRecoveryPhase(phase: ConnectionPhase): boolean {
  return phase === "disconnected" || phase === "error";
}

export interface ConnectOptions {
  resize_with_window?: boolean;
  /** Explicit pane selection. Root opens otherwise resolve the current first leaf. */
  terminal_id?: string;
}

interface ConnectionRequest {
  generation: number;
  session: SessionSummary;
  resume_from: string | null;
  resize_with_window: boolean;
  use_cached_state: boolean;
  on_complete?: (outcome: ConnectionOutcome) => void;
}

interface ConnectionOutcome {
  generation: number;
  error_code: string | null;
}

export interface AttachmentActions {
  state: AttachmentViewState;
  /** Advances for explicit opens/retries, never for automatic recovery. */
  connection_attempt: number;
  connect(session: SessionSummary, options?: ConnectOptions): Promise<void>;
  reconnect(): Promise<void>;
  cancelPendingConnection(session: SessionSummary): void;
  detach(): Promise<void>;
  /**
   * Forget local attachment state for a daemon restart. The restart command
   * owns backend detachment, so this deliberately does not send DetachAttachment.
   */
  resetAfterDaemonRestart(): void;
  handleInput(data: Uint8Array): void;
  toggleInputLease(): Promise<void>;
  toggleResizeWithWindow(): Promise<void>;
}

export function useAttachment(renderer: AttachmentRenderer | null, view_resize = false): AttachmentActions {
  const prepareManualReconnect = useManualReconnect();
  const view_resize_ref = useRef(view_resize);
  view_resize_ref.current = view_resize;
  const [state, publishState] = useState(INITIAL_STATE);
  const [manualReconnectPending, setManualReconnectPending] = useState(false);
  const connection_attempt = useRef(0);
  const stateRef = useRef(state);
  // Event channels can deliver multiple transitions before React commits.
  // Fence every callback against the latest transition, not the last render.
  const setState = useCallback((next: AttachmentViewState | ((current: AttachmentViewState) => AttachmentViewState)) => {
    const updated = typeof next === "function" ? next(stateRef.current) : next;
    stateRef.current = updated;
    publishState(updated);
  }, []);
  const rendererRef = useRef<AttachmentRenderer | null>(renderer);
  const activeAttachmentRef = useRef<string | null>(null);
  const openingAbortRef = useRef<AbortController | null>(null);
  const manualReconnectRef = useRef<{ abort: AbortController; promise: Promise<void> } | null>(null);
  const channelRef = useRef<Channel<AttachmentEvent> | null>(null);
  const generationRef = useRef(0);
  const eventTailRef = useRef(Promise.resolve());
  const appliedSequenceRef = useRef<string | null>(null);
  const pendingShellStateRef = useRef<ShellStateSummary | null>(null);
  const inputLeaseOwnedRef = useRef(false);
  const layoutLeaseOwnedRef = useRef(false);
  const resizeWithWindowRef = useRef(false);
  const lifecycleRecoveryStateRef = useRef<AttachmentViewState | null>(null);
  const recoveryBackoffRef = useRef(new AttachmentRecoveryBackoff());
  const recoveryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const drainDeferredConnectionRef = useRef<() => void>(() => {});
  const deferredConnectionRef = useRef<
    ConnectionIntentQueue<ConnectionRequest> | null
  >(null);
  const connectionQueueRef = useRef<LatestTaskQueue | null>(null);
  const layoutLeasePumpRef = useRef<LayoutLeasePump | null>(null);
  const resizePumpRef = useRef<ResizePump | null>(null);
  const resizeCoordinatorRef = useRef<ResizeCoordinator | null>(null);
  if (!connectionQueueRef.current) {
    connectionQueueRef.current = new LatestTaskQueue();
  }
  if (!deferredConnectionRef.current) {
    deferredConnectionRef.current = new ConnectionIntentQueue<ConnectionRequest>();
  }
  rendererRef.current = renderer;

  const abortManualReconnect = useCallback(() => {
    if (!manualReconnectRef.current) return;
    manualReconnectRef.current?.abort.abort();
    manualReconnectRef.current = null;
    setManualReconnectPending(false);
  }, []);

  const clearRecoveryTimer = useCallback(() => {
    if (recoveryTimerRef.current !== null) {
      clearTimeout(recoveryTimerRef.current);
      recoveryTimerRef.current = null;
    }
  }, []);

  const resetRecovery = useCallback(() => {
    clearRecoveryTimer();
    recoveryBackoffRef.current.reset();
  }, [clearRecoveryTimer]);

  const setFailure = useCallback((error: unknown) => {
    const code = errorCode(error);
    if (stateRef.current.phase === "ended") return;
    rendererRef.current?.invalidateResumeSequence();
    // An unusable actor cannot keep input/layout authority or publish late events.
    generationRef.current += 1;
    const attachment_id = activeAttachmentRef.current;
    activeAttachmentRef.current = null;
    channelRef.current = null;
    openingAbortRef.current?.abort();
    inputLeaseOwnedRef.current = false;
    layoutLeaseOwnedRef.current = false;
    inputPumpRef.current?.clear();
    layoutLeasePumpRef.current?.reset();
    resizeCoordinatorRef.current?.reset();
    if (attachment_id) void detachAttachment({ attachment_id }).catch(() => undefined);
    setState((current) => transitionAttachment(current, {
      type: "failed", code, message: errorMessage(error),
      resume_from: reconnectSequenceAfterError(code, current.reconnect_sequence),
    }));
  }, []);

  const stopResizeWithMessage = useCallback(
    (message: string, releaseLayout = false) => {
      resizeWithWindowRef.current = false;
      resizeCoordinatorRef.current?.stop();
      setState((current) => ({
        ...current,
        resize_with_window: false,
        message,
      }));

      const attachmentId = activeAttachmentRef.current;
      const generation = generationRef.current;
      if (releaseLayout && attachmentId) {
        layoutLeasePumpRef.current?.schedule({
          attachment_id: attachmentId,
          generation,
          acquire: false,
        });
      }
    },
    [],
  );

  if (!layoutLeasePumpRef.current) {
    layoutLeasePumpRef.current = new LayoutLeasePump(
      async (command) => {
        if (
          command.generation !== generationRef.current ||
          command.attachment_id !== activeAttachmentRef.current
        ) {
          return;
        }
        const request = {
          attachment_id: command.attachment_id,
          lease: "layout" as const,
        };
        if (command.acquire) {
          await acquireAttachmentLease(request);
        } else {
          await releaseAttachmentLease(request);
        }
      },
      (error, command) => {
        if (
          command.generation !== generationRef.current ||
          command.attachment_id !== activeAttachmentRef.current
        ) {
          return;
        }
        if (command.acquire && resizeWithWindowRef.current) {
          stopResizeWithMessage(
            `Could not acquire terminal layout: ${errorMessage(error)}`,
            true,
          );
        } else if (!command.acquire && !resizeWithWindowRef.current) {
          setState((current) => ({
            ...current,
            message: `Could not release terminal layout: ${errorMessage(error)}`,
          }));
        }
      },
    );
  }

  if (!resizePumpRef.current) {
    resizePumpRef.current = new ResizePump(
      async (resize) => {
        if (
          resize.generation !== generationRef.current ||
          resize.attachment_id !== activeAttachmentRef.current ||
          !resizeWithWindowRef.current ||
          !layoutLeaseOwnedRef.current
        ) {
          return;
        }
        await resizeAttachment({
          attachment_id: resize.attachment_id,
          terminal_size: resize.terminal_size,
        });
        if (view_resize_ref.current && resize.generation === generationRef.current) resizeCoordinatorRef.current?.setAuthoritative(resize.terminal_size);
      },
      (error, resize) => {
        if (
          resize.generation === generationRef.current &&
          resize.attachment_id === activeAttachmentRef.current
        ) {
          stopResizeWithMessage(
            `Could not resize terminal: ${errorMessage(error)}`,
            true,
          );
        }
      },
    );
  }

  if (!resizeCoordinatorRef.current) {
    resizeCoordinatorRef.current = new ResizeCoordinator(
      (requestedSize) => {
        const attachmentId = activeAttachmentRef.current;
        if (
          !attachmentId ||
          !resizeWithWindowRef.current ||
          !layoutLeaseOwnedRef.current
        ) {
          return;
        }
        resizePumpRef.current?.schedule({
          attachment_id: attachmentId,
          generation: generationRef.current,
          terminal_size: requestedSize,
        });
      },
      () => resizePumpRef.current?.clear(),
    );
  }

  const queueResize = useCallback((requestedSize: TerminalSize) => {
    if (!resizeWithWindowRef.current) {
      return;
    }
    resizeCoordinatorRef.current?.setDesired(requestedSize);
  }, []);

  const handleViewportResize = useCallback(
    (dimensions: ProposedDimensions) => {
      queueResize(terminalSize(dimensions.columns, dimensions.rows));
    },
    [queueResize],
  );

  useEffect(() => {
    if (
      !renderer ||
      state.phase !== "attached" ||
      !state.resize_with_window
    ) {
      return;
    }
    return renderer.observeDimensions(handleViewportResize);
  }, [handleViewportResize, renderer, state.phase, state.resize_with_window]);

  const inputPumpRef = useRef<InputPump | null>(null);
  if (!inputPumpRef.current) {
    inputPumpRef.current = new InputPump(
      async (data) => {
        const attachmentId = activeAttachmentRef.current;
        const generation = generationRef.current;
        if (!attachmentId || !inputLeaseOwnedRef.current) {
          return;
        }
        try {
          await sendInput({
            attachment_id: attachmentId,
            data_base64: encodeBase64(data),
          });
        } catch (error) {
          if (
            generation !== generationRef.current ||
            attachmentId !== activeAttachmentRef.current
          ) {
            return;
          }
          throw error;
        }
      },
      setFailure,
    );
  }

  useEffect(() => {
    if (lifecycleRecoveryStateRef.current) {
      const recoveryState = lifecycleRecoveryStateRef.current;
      lifecycleRecoveryStateRef.current = null;
      stateRef.current = recoveryState;
      setState(recoveryState);
    }
    return () => {
      abortManualReconnect();
      lifecycleRecoveryStateRef.current =
        interruptedAttachmentState(stateRef.current) ?? INITIAL_STATE;
      clearRecoveryTimer();
      generationRef.current += 1;
      openingAbortRef.current?.abort();
      inputLeaseOwnedRef.current = false;
      layoutLeaseOwnedRef.current = false;
      resizeWithWindowRef.current = false;
      inputPumpRef.current?.clear();
      layoutLeasePumpRef.current?.reset();
      resizeCoordinatorRef.current?.reset();
      deferredConnectionRef.current?.cancel();
      connectionQueueRef.current?.cancelPending();
      appliedSequenceRef.current = null;
      pendingShellStateRef.current = null;
      const attachmentId = activeAttachmentRef.current;
      activeAttachmentRef.current = null;
      channelRef.current = null;
      if (attachmentId) {
        void detachAttachment({ attachment_id: attachmentId });
      }
    };
  }, [abortManualReconnect, clearRecoveryTimer]);

  const publishAppliedSequence = useCallback((sequence: string) => {
    appliedSequenceRef.current = sequence;
    setState((current) => ({ ...current, applied_sequence: sequence }));
    const pending = pendingShellStateRef.current;
    if (pending && sequenceAtLeast(sequence, pending.observed_sequence)) {
      pendingShellStateRef.current = null;
      setState((current) => ({ ...current, shell_state: pending }));
    }
  }, []);

  const publishShellState = useCallback((shellState: ShellStateSummary) => {
    if (sequenceAtLeast(appliedSequenceRef.current, shellState.observed_sequence)) {
      pendingShellStateRef.current = null;
      setState((current) => ({ ...current, shell_state: shellState }));
      return;
    }
    const pending = pendingShellStateRef.current;
    if (!pending || BigInt(shellState.revision) > BigInt(pending.revision)) {
      pendingShellStateRef.current = shellState;
    }
  }, []);

  const acknowledge = useCallback(async (attachmentId: string, eventId: string) => {
    await acknowledgeAttachmentEvent({
      attachment_id: attachmentId,
      event_id: eventId,
    });
  }, []);

  const processEvent = useCallback(
    async (event: AttachmentEvent, generation: number) => {
      const isCurrent = () =>
        generation === generationRef.current &&
        event.attachment_id === activeAttachmentRef.current;
      if (!isCurrent()) {
        return;
      }
      if (stateRef.current.phase === "ended" && event.event_type !== "attachment_exited") return;
      if (!renderer) {
        throw new Error("The terminal renderer is not available.");
      }

      switch (event.event_type) {
        case "checkpoint":
          await renderer.restoreCheckpoint(
            event.checkpoint.terminal_size,
            event.history.lines,
            decodeBase64(event.checkpoint.payload_base64),
            decodeBase64(event.checkpoint.input_prefix_base64),
            event.checkpoint.sequence,
          );
          if (!isCurrent()) {
            return;
          }
          await acknowledge(event.attachment_id, event.event_id);
          if (!isCurrent()) {
            return;
          }
          publishAppliedSequence(event.checkpoint.sequence);
          if (!view_resize_ref.current) resizeCoordinatorRef.current?.setAuthoritative(
            event.checkpoint.terminal_size,
          );
          setState((current) => ({
            ...current,
            history_gap: current.history_gap || event.history_gap,
            session: current.session
              ? {
                  ...current.session,
                  terminal_size: event.checkpoint.terminal_size,
                }
              : current.session,
          }));
          break;
        case "output":
          await renderer.write(decodeBase64(event.data_base64), event.sequence_end);
          if (!isCurrent()) {
            return;
          }
          await acknowledge(event.attachment_id, event.event_id);
          if (!isCurrent()) {
            return;
          }
          publishAppliedSequence(event.sequence_end);
          break;
        case "pty_geometry_changed":
          await renderer.resize(event.terminal_size);
          if (!isCurrent()) {
            return;
          }
          await acknowledge(event.attachment_id, event.event_id);
          if (!isCurrent()) {
            return;
          }
          if (!view_resize_ref.current) resizeCoordinatorRef.current?.setAuthoritative(event.terminal_size);
          setState((current) => ({
            ...current,
            session: current.session
              ? { ...current.session, terminal_size: event.terminal_size }
              : current.session,
          }));
          break;
        case "lease_status":
          const expectedLayoutIntent =
            event.lease === "layout"
              ? layoutLeasePumpRef.current?.takeExpectedResponse(
                  event.attachment_id,
                  generation,
                ) ?? null
              : null;
          if (event.lease === "input") {
            inputLeaseOwnedRef.current = event.status.owned_by_client;
          }
          if (event.lease === "layout") {
            layoutLeaseOwnedRef.current = event.status.owned_by_client;
            if (!event.status.owned_by_client) {
              resizeCoordinatorRef.current?.setEnabled(false);
            }
          }
          const resizeLost =
            event.lease === "layout" &&
            shouldStopResizeAfterLeaseStatus(
              resizeWithWindowRef.current,
              event.status.owned_by_client,
              expectedLayoutIntent,
            );
          if (resizeLost) {
            resizeWithWindowRef.current = false;
            resizeCoordinatorRef.current?.stop();
          }
          setState((current) => ({
            ...current,
            input_lease:
              event.lease === "input" ? event.status : current.input_lease,
            layout_lease:
              event.lease === "layout" ? event.status : current.layout_lease,
            resize_with_window: resizeLost
              ? false
              : current.resize_with_window,
            message: resizeLost
              ? event.status.held
                ? "Another client controls this session's terminal size."
                : "Resize with window stopped because layout ownership was released."
              : current.message,
          }));
          if (
            event.lease === "layout" &&
            event.status.owned_by_client &&
            resizeWithWindowRef.current
          ) {
            const proposed = renderer.proposeDimensions();
            if (proposed) {
              queueResize(terminalSize(proposed.columns, proposed.rows));
            }
            resizeCoordinatorRef.current?.setEnabled(true);
          } else if (
            event.lease === "layout" &&
            event.status.owned_by_client &&
            !resizeWithWindowRef.current &&
            !layoutLeasePumpRef.current?.hasScheduledIntent(
              event.attachment_id,
              generation,
              false,
            )
          ) {
            layoutLeasePumpRef.current?.schedule({
              attachment_id: event.attachment_id,
              generation,
              acquire: false,
            });
          }
          break;
        case "shell_state_changed":
          publishShellState(event.shell_state);
          break;
        case "server_error":
          setState((current) => ({ ...current, message: event.message }));
          break;
        case "session_ended":
          resetRecovery();
          inputLeaseOwnedRef.current = false;
          layoutLeaseOwnedRef.current = false;
          resizeWithWindowRef.current = false;
          layoutLeasePumpRef.current?.reset();
          resizeCoordinatorRef.current?.stop();
          setState((current) => transitionAttachment(current, { type: "ended", exit_code: event.exit_code }));
          break;
        case "attachment_exited":
          if (event.next_sequence === null) renderer.invalidateResumeSequence();
          const resumeResize =
            event.reason === "connection_closed" && resizeWithWindowRef.current;
          if (event.reason !== "connection_closed") {
            resetRecovery();
          }
          activeAttachmentRef.current = null;
          channelRef.current = null;
          inputLeaseOwnedRef.current = false;
          layoutLeaseOwnedRef.current = false;
          resizeWithWindowRef.current = resumeResize;
          inputPumpRef.current?.clear();
          layoutLeasePumpRef.current?.reset();
          resizeCoordinatorRef.current?.reset();
          setState((current) => transitionAttachment(current, {
            type: "closed", reason: event.reason, next_sequence: event.next_sequence,
          }));
          break;
        case "attachment_error":
          renderer.invalidateResumeSequence();
          activeAttachmentRef.current = null;
          channelRef.current = null;
          inputLeaseOwnedRef.current = false;
          layoutLeaseOwnedRef.current = false;
          inputPumpRef.current?.clear();
          layoutLeasePumpRef.current?.reset();
          resizeCoordinatorRef.current?.reset();
          setState((current) => transitionAttachment(current, {
            type: "failed", code: event.code, message: event.message, resume_from: null,
          }));
          break;
      }
    },
    [
      acknowledge,
      publishAppliedSequence,
      publishShellState,
      queueResize,
      renderer,
      resetRecovery,
    ],
  );

  const queueEvent = useCallback(
    (event: AttachmentEvent, generation: number) => {
      const next = eventTailRef.current.then(() => processEvent(event, generation));
      eventTailRef.current = next.catch((error) => {
        if (
          generation === generationRef.current &&
          event.attachment_id === activeAttachmentRef.current
        ) {
          setFailure(error);
        }
      });
    },
    [processEvent, setFailure],
  );

  const performConnection = useCallback(
    async (request: ConnectionRequest) => {
      const { generation, session, resize_with_window: resizeWithWindow } = request;
      if (generation !== generationRef.current || !renderer) {
        return;
      }

      // Finish any write already handed to xterm before reading its saved
      // cursor. Queued events from the old generation are ignored.
      await eventTailRef.current;
      if (generation !== generationRef.current) return;
      renderer.activateSession(session);
      // A server cursor is safe only with its fully applied local screen. A
      // replaced cache or an unacknowledged write requires a fresh checkpoint.
      const retainedSequence = renderer.resumeSequence();
      let resumeFrom = request.use_cached_state
        ? retainedSequence
        : request.resume_from === retainedSequence ? request.resume_from : null;
      let restored_local_cache = false;
      try {
        if (resumeFrom === null && renderer.resumeSequence() === null) {
          const response = await sessionCache({ kind: "load", host_key: targetKey(session.target), session_id: session.session_id, terminal_id: session.terminal_id });
          if (generation !== generationRef.current) return;
          if (response.kind === "loaded" && response.cache) {
            const cached = response.cache;
            await renderer.restoreCheckpoint(cached.checkpoint.terminal_size, cached.history,
              decodeBase64(cached.checkpoint.payload_base64), decodeBase64(cached.checkpoint.input_prefix_base64), cached.checkpoint.sequence);
            if (generation !== generationRef.current) return;
            resumeFrom = cached.checkpoint.sequence;
            restored_local_cache = true;
            setState((current) => ({ ...current, history_gap: cached.history_gap }));
          }
        }
      } catch (error) {
        if (generation !== generationRef.current) return;
        setFailure({ code: "local_cache_failed", message: errorMessage(error) });
        return;
      }
      appliedSequenceRef.current = resumeFrom;
      setState((current) => ({
        ...current,
        applied_sequence: resumeFrom,
        reconnect_sequence: resumeFrom,
      }));

      const previous_attachment_id = activeAttachmentRef.current;
      activeAttachmentRef.current = null;
      channelRef.current = null;

      const proposed = renderer.proposeDimensions();
      const requestedTerminalSize = proposed
        ? terminalSize(proposed.columns, proposed.rows)
        : terminalSize(80, 24);
      let pendingEvents: AttachmentEvent[] = [];
      let responseReady = false;
      let openingAttempt = 0;
      const openingAbort = new AbortController();
      openingAbortRef.current = openingAbort;
      let unclaimed_attachment_id: string | null = null;
      try {
        // Each renderer owns its attachment. Replacing this renderer's session
        // must release its leases without detaching sibling panes.
        if (previous_attachment_id) {
          await detachAttachment({ attachment_id: previous_attachment_id });
          if (generation !== generationRef.current) return;
        }
        let openingSession = session;
        // Disk content is a preview. A root pane may have changed since it was
        // saved, so it always requires an authoritative checkpoint.
        let requestedResume = restored_local_cache ? null : resumeFrom;
        let result!: Awaited<ReturnType<typeof openAttachment>>;
        for (let attempt = 0; attempt < 2; attempt += 1) {
          openingAttempt = attempt;
          result = await openAttachment(
            {
              target: openingSession.target,
              session: openingSession.terminal_id ?? openingSession.session_id,
              resume_from: requestedResume,
              terminal_size: requestedTerminalSize,
              request_input_lease: true,
              request_layout_lease: resizeWithWindow,
            },
            (event) => {
              if (generation !== generationRef.current || attempt !== openingAttempt) return;
              if (responseReady) queueEvent(event, generation);
              else pendingEvents.push(event);
            },
            openingAbort.signal,
          );
          unclaimed_attachment_id = result.attached.attachment_id;
          if (generation !== generationRef.current) return;
          renderer.adoptSession(result.attached.session);
          if (requestedResume === null || renderer.resumeSequence() === requestedResume) break;

          // The pane may have moved to another session, or its cache may have
          // disappeared during open. Discard this delta-only stream and retry
          // once against the resolved terminal with a fresh checkpoint.
          openingAttempt += 1;
          pendingEvents = [];
          await detachAttachment({ attachment_id: unclaimed_attachment_id });
          unclaimed_attachment_id = null;
          if (generation !== generationRef.current) return;
          openingSession = result.attached.session;
          requestedResume = null;
          resumeFrom = null;
          appliedSequenceRef.current = null;
          setState((current) => ({ ...current, session: openingSession, applied_sequence: null, reconnect_sequence: null }));
        }
        if (resumeFrom === null) {
          await renderer.recreate(result.attached.session.terminal_size);
          if (generation !== generationRef.current) {
            return;
          }
          appliedSequenceRef.current = null;
        }
        activeAttachmentRef.current = result.attached.attachment_id;
        unclaimed_attachment_id = null;
        channelRef.current = result.channel;
        inputLeaseOwnedRef.current = result.attached.input_lease.owned_by_client;
        layoutLeaseOwnedRef.current = result.attached.layout_lease.owned_by_client;
        const resizeActive =
          resizeWithWindow && result.attached.layout_lease.owned_by_client;
        resizeWithWindowRef.current = resizeActive;
        resizeCoordinatorRef.current?.reset(
          view_resize_ref.current ? (resizeActive ? requestedTerminalSize : null) : result.attached.session.terminal_size,
        );
        if (resizeActive) {
          resizeCoordinatorRef.current?.setDesired(requestedTerminalSize);
          resizeCoordinatorRef.current?.setEnabled(true);
        }
        publishShellState(result.attached.shell_state);
        setState((current) => transitionAttachment(current, {
          type: "attached", response: result.attached, resize_with_window: resizeWithWindow,
        }));
        responseReady = true;
        for (const event of pendingEvents) {
          queueEvent(event, generation);
        }
        request.on_complete?.({ generation, error_code: null });
        renderer.focus();
      } catch (error) {
        if (generation === generationRef.current) {
          setFailure(error);
          request.on_complete?.({ generation: generationRef.current, error_code: errorCode(error) });
        }
      } finally {
        if (unclaimed_attachment_id !== null) {
          await detachAttachment({ attachment_id: unclaimed_attachment_id }).catch(() => undefined);
        }
        if (openingAbortRef.current === openingAbort) {
          openingAbortRef.current = null;
        }
      }
    },
    [publishShellState, queueEvent, renderer, setFailure],
  );

  const submitConnection = useCallback(
    (request: ConnectionRequest): Promise<void> =>
      connectionQueueRef.current!.submit(
        () => performConnection(request),
        (error) => {
          if (request.generation === generationRef.current) {
            setFailure(error);
          }
        },
      ),
    [performConnection, setFailure],
  );

  const drainDeferredConnection = useCallback(() => {
    if (!rendererRef.current) {
      return;
    }
    deferredConnectionRef.current!.drain(submitConnection);
  }, [submitConnection]);
  drainDeferredConnectionRef.current = drainDeferredConnection;

  useEffect(() => {
    drainDeferredConnection();
  }, [drainDeferredConnection, renderer]);

  const connectAt = useCallback(
    (
      session: SessionSummary,
      resumeFrom: string | null,
      resizeWithWindow: boolean,
      use_cached_state = false,
      intent: ConnectionIntent = "attach",
      on_complete?: (outcome: ConnectionOutcome) => void,
    ): Promise<void> => {
      const generation = generationRef.current + 1;
      generationRef.current = generation;
      openingAbortRef.current?.abort();
      inputLeaseOwnedRef.current = false;
      layoutLeaseOwnedRef.current = false;
      resizeWithWindowRef.current = resizeWithWindow;
      inputPumpRef.current?.clear();
      layoutLeasePumpRef.current?.reset();
      resizeCoordinatorRef.current?.reset(view_resize_ref.current ? null : session.terminal_size);
      pendingShellStateRef.current = null;
      rendererRef.current?.activateSession(session);
      if (use_cached_state) {
        resumeFrom = rendererRef.current?.resumeSequence() ?? null;
      }
      appliedSequenceRef.current = resumeFrom;
      const nextState = transitionAttachment(stateRef.current, {
        type: "begin", intent, session, resume_from: resumeFrom, resize_with_window: resizeWithWindow,
      });
      stateRef.current = nextState;
      setState(nextState);

      const request = {
        generation,
        session,
        resume_from: resumeFrom,
        resize_with_window: resizeWithWindow,
        use_cached_state,
        on_complete,
      };
      deferredConnectionRef.current!.begin(request);
      const completion = deferredConnectionRef.current!.defer(request);
      drainDeferredConnectionRef.current();
      return completion;
    },
    [],
  );

  const connect = useCallback(
    async (session: SessionSummary, options: ConnectOptions = {}) => {
      abortManualReconnect();
      connection_attempt.current += 1;
      resetRecovery();
      const selected = { ...session, terminal_id: options.terminal_id };
      return connectAt(selected, null, options.resize_with_window ?? view_resize_ref.current, Boolean(options.terminal_id));
    },
    [abortManualReconnect, connectAt, resetRecovery],
  );

  const reconnectCurrent = useCallback(
    async (resetBackoff: boolean): Promise<ConnectionOutcome | null> => {
      const current = stateRef.current;
      if (!current.session || current.phase === "ended" || (!resetBackoff && manualReconnectRef.current)) {
        return null;
      }
      if (resetBackoff) {
        connection_attempt.current += 1;
        resetRecovery();
      }

      const attachmentId = activeAttachmentRef.current;
      let transitionGeneration = generationRef.current;
      if (attachmentId) {
        generationRef.current += 1;
        transitionGeneration = generationRef.current;
        activeAttachmentRef.current = null;
        channelRef.current = null;
        try {
          await detachAttachment({ attachment_id: attachmentId });
        } catch {
          // A failed actor is often already closing. The subsequent open is
          // authoritative and its stable error code drives the retry policy.
        }
      }
      if (
        transitionGeneration !== generationRef.current ||
        !sameSession(stateRef.current.session, current.session)
      ) {
        return null;
      }
      const completion: { outcome: ConnectionOutcome | null } = { outcome: null };
      await connectAt(
        current.session,
        current.reconnect_sequence,
        current.resize_with_window,
        false,
        "reconnect",
        (outcome) => { completion.outcome = outcome; },
      );
      return completion.outcome?.generation === generationRef.current ? completion.outcome : null;
    },
    [connectAt, resetRecovery],
  );

  const reconnect = useCallback(
    (): Promise<void> => {
      if (manualReconnectRef.current) return manualReconnectRef.current.promise;
      const session = stateRef.current.session;
      if (!session || stateRef.current.phase === "ended") return Promise.resolve();
      if (session.target.kind !== "ssh" || !prepareManualReconnect) {
        return reconnectCurrent(true).then(() => undefined);
      }
      resetRecovery();
      let generation = generationRef.current;
      const pending = { abort: new AbortController(), promise: Promise.resolve() };
      manualReconnectRef.current = pending;
      setManualReconnectPending(true);
      const isCurrent = () => {
        const current = stateRef.current.session;
        return manualReconnectRef.current === pending && !pending.abort.signal.aborted &&
          generationRef.current === generation && current !== null && sameSession(current, session) &&
          current.terminal_id === session.terminal_id && sameSshEndpoint(current.target, session.target);
      };
      const prepared = (connected: boolean) => {
        if (!isCurrent()) return false;
        if (!connected) setState((current) => activeAttachmentRef.current
          ? { ...current, message: "Reconnect cancelled." }
          : transitionAttachment(current, {
            type: "failed", code: "attachment_cancelled", message: "Reconnect cancelled.",
            resume_from: current.reconnect_sequence,
          }));
        return connected;
      };
      pending.promise = (async () => {
        try {
          if (!prepared(await prepareManualReconnect(session.target, pending.abort.signal))) return;
          const outcome = await reconnectCurrent(true);
          if (!outcome || manualReconnectRef.current !== pending || pending.abort.signal.aborted) return;
          generation = outcome.generation;
          if (!isCurrent() || !["ssh_authentication_required", "ssh_host_disconnected"].includes(outcome.error_code ?? "")) return;
          // The master may disappear after preflight. One explicit retry can
          // authenticate again; ordinary transport failures never prompt here.
          if (!prepared(await prepareManualReconnect(session.target, pending.abort.signal, true))) return;
          await reconnectCurrent(true);
        } catch (error) {
          if (isCurrent()) setState((current) => activeAttachmentRef.current
            ? { ...current, message: errorMessage(error) }
            : transitionAttachment(current, {
              type: "failed", code: errorCode(error), message: errorMessage(error),
              resume_from: current.reconnect_sequence,
            }));
        } finally {
          if (manualReconnectRef.current === pending) {
            manualReconnectRef.current = null;
            setManualReconnectPending(false);
          }
        }
      })();
      return pending.promise;
    },
    [prepareManualReconnect, reconnectCurrent, resetRecovery, setState],
  );

  const recovery_session_key = state.session ? sessionKey(state.session) : null;
  useEffect(() => {
    if (manualReconnectRef.current) return;
    if (state.phase === "attached" && recoveryBackoffRef.current.isActive()) {
      recoveryTimerRef.current = setTimeout(() => {
        recoveryTimerRef.current = null;
        recoveryBackoffRef.current.reset();
      }, ATTACHMENT_RECOVERY_STABILITY_MS);

      return () => {
        if (recoveryTimerRef.current !== null) {
          clearTimeout(recoveryTimerRef.current);
          recoveryTimerRef.current = null;
        }
      };
    }

    if (!state.session) return;
    if (matchesRecoveryPhase(state.phase) && canAutomaticallyRecoverAttachment(state.error_code)) {
      const now = Date.now();
      const delay = recoveryBackoffRef.current.nextDelay(now);
      setState((current) => transitionAttachment(current, delay === null
        ? { type: "retry_exhausted" }
        : { type: "retry_scheduled", retry_at_ms: now + delay }));
      return;
    }
    if (state.phase !== "retry_wait" || state.retry_at_ms == null) return;

    const identity = sessionKey(state.session);
    recoveryTimerRef.current = setTimeout(() => {
      recoveryTimerRef.current = null;
      const current = stateRef.current;
      if (current.session === null || sessionKey(current.session) !== identity ||
        current.phase !== "retry_wait" || current.retry_at_ms !== state.retry_at_ms) return;
      if (recoveryBackoffRef.current.isExpired(Date.now())) {
        setState((current) => transitionAttachment(current, { type: "retry_exhausted" }));
        return;
      }
      void reconnectCurrent(false);
    }, Math.max(0, state.retry_at_ms - Date.now()));
    return clearRecoveryTimer;
  }, [
    reconnectCurrent, clearRecoveryTimer, state.attachment_id, state.error_code,
    state.phase, state.retry_at_ms, recovery_session_key, manualReconnectPending,
  ]);

  const cancelPendingConnection = useCallback((session: SessionSummary) => {
    const identity = sessionKey(session);
    if (sameSession(stateRef.current.session, session)) abortManualReconnect();
    const cancelled = deferredConnectionRef.current?.cancelIf(
      (request) => sessionKey(request.session) === identity,
    );
    if (!cancelled) {
      return;
    }

    resetRecovery();
    generationRef.current += 1;
    openingAbortRef.current?.abort();
    connectionQueueRef.current?.cancelPending();
    appliedSequenceRef.current = null;
    pendingShellStateRef.current = null;
    setState(INITIAL_STATE);
  }, [abortManualReconnect, resetRecovery]);

  const detach = useCallback(async () => {
    abortManualReconnect();
    resetRecovery();
    const generation = generationRef.current + 1;
    generationRef.current = generation;
    openingAbortRef.current?.abort();
    inputLeaseOwnedRef.current = false;
    layoutLeaseOwnedRef.current = false;
    resizeWithWindowRef.current = false;
    inputPumpRef.current?.clear();
    layoutLeasePumpRef.current?.reset();
    resizeCoordinatorRef.current?.reset();
    deferredConnectionRef.current?.cancel();
    connectionQueueRef.current?.cancelPending();
    const attachmentId = activeAttachmentRef.current;
    activeAttachmentRef.current = null;
    channelRef.current = null;
    if (attachmentId) {
      try {
        await detachAttachment({ attachment_id: attachmentId });
      } catch (error) {
        if (generation === generationRef.current) {
          setState((current) => ({ ...transitionAttachment(current, {
            type: "failed", code: "explicit_detach_failed", message: errorMessage(error), resume_from: null,
          }), resize_with_window: false }));
        }
        return;
      }
    }
    if (generation !== generationRef.current) {
      return;
    }
    appliedSequenceRef.current = null;
    pendingShellStateRef.current = null;
    setState(INITIAL_STATE);
  }, [abortManualReconnect, resetRecovery]);

  const resetAfterDaemonRestart = useCallback(() => {
    abortManualReconnect();
    resetRecovery();
    generationRef.current += 1;
    openingAbortRef.current?.abort();
    inputLeaseOwnedRef.current = false;
    layoutLeaseOwnedRef.current = false;
    resizeWithWindowRef.current = false;
    inputPumpRef.current?.clear();
    layoutLeasePumpRef.current?.reset();
    resizeCoordinatorRef.current?.reset();
    deferredConnectionRef.current?.cancel();
    connectionQueueRef.current?.cancelPending();
    appliedSequenceRef.current = null;
    pendingShellStateRef.current = null;
    activeAttachmentRef.current = null;
    channelRef.current = null;
    stateRef.current = INITIAL_STATE;
    setState(INITIAL_STATE);
  }, [abortManualReconnect, resetRecovery]);

  useEffect(() => registerAttachmentControl({
    attachmentId: () => activeAttachmentRef.current,
    session: () => stateRef.current.session,
    reconnect: async (expected_id) => {
      if (activeAttachmentRef.current !== expected_id) return null;
      abortManualReconnect();
      await reconnectCurrent(true);
      const replacement = activeAttachmentRef.current;
      return replacement !== expected_id ? replacement : null;
    },
    reset: resetAfterDaemonRestart,
  }), [abortManualReconnect, reconnectCurrent, resetAfterDaemonRestart]);

  const handleInput = useCallback((data: Uint8Array) => {
    if (!inputLeaseOwnedRef.current || !activeAttachmentRef.current) {
      return;
    }
    if (!inputPumpRef.current?.push(data)) {
      setState((current) => ({
        ...current,
        message: "Terminal input paused because the local input queue is full.",
      }));
    }
  }, []);

  const changeLease = useCallback(
    async (lease: LeaseKind, acquire: boolean) => {
      const attachmentId = activeAttachmentRef.current;
      const generation = generationRef.current;
      if (!attachmentId) {
        return;
      }
      const request = { attachment_id: attachmentId, lease };
      try {
        if (acquire) {
          await acquireAttachmentLease(request);
        } else {
          await releaseAttachmentLease(request);
        }
      } catch (error) {
        if (
          generation === generationRef.current &&
          attachmentId === activeAttachmentRef.current
        ) {
          setFailure(error);
        }
      }
    },
    [setFailure],
  );

  const toggleInputLease = useCallback(
    async () => changeLease("input", !stateRef.current.input_lease.owned_by_client),
    [changeLease],
  );

  const toggleResizeWithWindow = useCallback(async () => {
    const attachmentId = activeAttachmentRef.current;
    const generation = generationRef.current;
    if (!attachmentId) {
      return;
    }

    if (resizeWithWindowRef.current) {
      resizeWithWindowRef.current = false;
      resizeCoordinatorRef.current?.stop();
      setState((current) => ({
        ...current,
        resize_with_window: false,
        message: null,
      }));
      layoutLeasePumpRef.current?.schedule({
        attachment_id: attachmentId,
        generation,
        acquire: false,
      });
      return;
    }

    const proposed = renderer?.proposeDimensions();
    if (!proposed) {
      setState((current) => ({
        ...current,
        message: "The terminal viewport is not ready for layout measurement.",
      }));
      return;
    }

    const requestedSize = terminalSize(proposed.columns, proposed.rows);
    resizeWithWindowRef.current = true;
    queueResize(requestedSize);
    setState((current) => ({
      ...current,
      resize_with_window: true,
      message: null,
    }));

    layoutLeasePumpRef.current?.schedule({
      attachment_id: attachmentId,
      generation,
      acquire: true,
    });
  }, [queueResize, renderer]);

  return {
    state,
    connection_attempt: connection_attempt.current,
    connect,
    reconnect,
    cancelPendingConnection,
    detach,
    resetAfterDaemonRestart,
    handleInput,
    toggleInputLease,
    toggleResizeWithWindow,
  };
}
