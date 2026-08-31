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
} from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type {
  AttachmentEvent,
  AttachmentViewState,
  LeaseKind,
  SessionSummary,
  ShellStateSummary,
  TerminalSize,
} from "../../lib/types";
import type { ProposedDimensions } from "../terminal/TerminalPresenter";
import type { XtermRenderer } from "../terminal/XtermRenderer";
import { InputPump } from "./InputPump";
import {
  LayoutLeasePump,
  shouldStopResizeAfterLeaseStatus,
} from "./LayoutLeasePump";
import { LatestTaskQueue } from "./LatestTaskQueue";
import { ResizeCoordinator } from "./ResizeCoordinator";
import { ResizePump } from "./ResizePump";

const EMPTY_LEASE = { held: false, owned_by_client: false };

const INITIAL_STATE: AttachmentViewState = {
  phase: "idle",
  attachment_id: null,
  session: null,
  input_lease: EMPTY_LEASE,
  layout_lease: EMPTY_LEASE,
  shell_state: null,
  applied_sequence: null,
  reconnect_sequence: null,
  history_gap: false,
  terminal_size_mismatch: false,
  resize_with_window: false,
  message: null,
};

function terminalSize(columns: number, rows: number): TerminalSize {
  return {
    columns,
    rows,
    pixel_width: null,
    pixel_height: null,
  };
}

export interface ConnectOptions {
  resize_with_window?: boolean;
}

interface ConnectionRequest {
  generation: number;
  session: SessionSummary;
  resumeFrom: string | null;
  resizeWithWindow: boolean;
}

export interface AttachmentActions {
  state: AttachmentViewState;
  connect(session: SessionSummary, options?: ConnectOptions): Promise<void>;
  reconnect(): Promise<void>;
  detach(): Promise<void>;
  handleInput(data: Uint8Array): void;
  toggleInputLease(): Promise<void>;
  toggleResizeWithWindow(): Promise<void>;
}

export function useAttachment(renderer: XtermRenderer | null): AttachmentActions {
  const [state, setState] = useState(INITIAL_STATE);
  const stateRef = useRef(state);
  const activeAttachmentRef = useRef<string | null>(null);
  const channelRef = useRef<Channel<AttachmentEvent> | null>(null);
  const generationRef = useRef(0);
  const eventTailRef = useRef(Promise.resolve());
  const appliedSequenceRef = useRef<string | null>(null);
  const pendingShellStateRef = useRef<ShellStateSummary | null>(null);
  const inputLeaseOwnedRef = useRef(false);
  const layoutLeaseOwnedRef = useRef(false);
  const resizeWithWindowRef = useRef(false);
  const connectionQueueRef = useRef<LatestTaskQueue | null>(null);
  const layoutLeasePumpRef = useRef<LayoutLeasePump | null>(null);
  const resizePumpRef = useRef<ResizePump | null>(null);
  const resizeCoordinatorRef = useRef<ResizeCoordinator | null>(null);
  if (!connectionQueueRef.current) {
    connectionQueueRef.current = new LatestTaskQueue();
  }

  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  const setFailure = useCallback((error: unknown) => {
    setState((current) => ({
      ...current,
      phase: "error",
      message: errorMessage(error),
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
    return () => {
      generationRef.current += 1;
      inputLeaseOwnedRef.current = false;
      layoutLeaseOwnedRef.current = false;
      resizeWithWindowRef.current = false;
      inputPumpRef.current?.clear();
      layoutLeasePumpRef.current?.reset();
      resizeCoordinatorRef.current?.reset();
      connectionQueueRef.current?.cancelPending();
      const attachmentId = activeAttachmentRef.current;
      activeAttachmentRef.current = null;
      channelRef.current = null;
      if (attachmentId) {
        void detachAttachment({ attachment_id: attachmentId });
      }
    };
  }, []);

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
      if (!renderer) {
        throw new Error("The terminal renderer is not available.");
      }

      switch (event.event_type) {
        case "checkpoint":
          await renderer.restoreCheckpoint(
            event.checkpoint.terminal_size,
            decodeBase64(event.checkpoint.payload_base64),
            decodeBase64(event.checkpoint.input_prefix_base64),
          );
          if (!isCurrent()) {
            return;
          }
          await acknowledge(event.attachment_id, event.event_id);
          if (!isCurrent()) {
            return;
          }
          publishAppliedSequence(event.checkpoint.sequence);
          resizeCoordinatorRef.current?.setAuthoritative(
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
          await renderer.write(decodeBase64(event.data_base64));
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
          resizeCoordinatorRef.current?.setAuthoritative(event.terminal_size);
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
          inputLeaseOwnedRef.current = false;
          layoutLeaseOwnedRef.current = false;
          resizeWithWindowRef.current = false;
          layoutLeasePumpRef.current?.reset();
          resizeCoordinatorRef.current?.stop();
          setState((current) => ({
            ...current,
            phase: "ended",
            input_lease: EMPTY_LEASE,
            layout_lease: EMPTY_LEASE,
            resize_with_window: false,
            message:
              event.exit_code === null
                ? "Session ended."
                : `Session ended with exit code ${event.exit_code}.`,
          }));
          break;
        case "attachment_exited":
          const resumeResize =
            event.reason === "connection_closed" && resizeWithWindowRef.current;
          activeAttachmentRef.current = null;
          channelRef.current = null;
          inputLeaseOwnedRef.current = false;
          layoutLeaseOwnedRef.current = false;
          resizeWithWindowRef.current = resumeResize;
          inputPumpRef.current?.clear();
          layoutLeasePumpRef.current?.reset();
          resizeCoordinatorRef.current?.reset();
          setState((current) => ({
            ...current,
            attachment_id: null,
            input_lease: EMPTY_LEASE,
            layout_lease: EMPTY_LEASE,
            resize_with_window: resumeResize,
            phase:
              event.reason === "session_ended"
                ? "ended"
                : event.reason === "detached"
                  ? "idle"
                  : "disconnected",
            reconnect_sequence: event.next_sequence,
            message:
              event.reason === "connection_closed"
                ? "Connection closed. The rmux session may still be running."
                : current.message,
          }));
          break;
        case "attachment_error":
          activeAttachmentRef.current = null;
          channelRef.current = null;
          inputLeaseOwnedRef.current = false;
          layoutLeaseOwnedRef.current = false;
          inputPumpRef.current?.clear();
          layoutLeasePumpRef.current?.reset();
          resizeCoordinatorRef.current?.reset();
          setState((current) => ({
            ...current,
            attachment_id: null,
            input_lease: EMPTY_LEASE,
            layout_lease: EMPTY_LEASE,
            phase: "error",
            reconnect_sequence: null,
            message: event.message,
          }));
          break;
      }
    },
    [acknowledge, publishAppliedSequence, publishShellState, queueResize, renderer],
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
      const { generation, session, resumeFrom, resizeWithWindow } = request;
      if (generation !== generationRef.current || !renderer) {
        return;
      }

      activeAttachmentRef.current = null;
      channelRef.current = null;

      const proposed = renderer.proposeDimensions();
      const requestedTerminalSize = proposed
        ? terminalSize(proposed.columns, proposed.rows)
        : terminalSize(80, 24);
      const pendingEvents: AttachmentEvent[] = [];
      let responseReady = false;
      try {
        const result = await openAttachment(
          {
            session: session.session_id,
            resume_from: resumeFrom,
            terminal_size: requestedTerminalSize,
            request_input_lease: true,
            request_layout_lease: resizeWithWindow,
          },
          (event) => {
            if (responseReady) {
              queueEvent(event, generation);
            } else {
              pendingEvents.push(event);
            }
          },
        );

        if (generation !== generationRef.current) {
          await detachAttachment({
            attachment_id: result.attached.attachment_id,
          });
          return;
        }
        if (!resumeFrom) {
          await renderer.recreate(result.attached.session.terminal_size);
          if (generation !== generationRef.current) {
            await detachAttachment({
              attachment_id: result.attached.attachment_id,
            });
            return;
          }
          appliedSequenceRef.current = null;
        }
        activeAttachmentRef.current = result.attached.attachment_id;
        channelRef.current = result.channel;
        inputLeaseOwnedRef.current = result.attached.input_lease.owned_by_client;
        layoutLeaseOwnedRef.current = result.attached.layout_lease.owned_by_client;
        const resizeActive =
          resizeWithWindow && result.attached.layout_lease.owned_by_client;
        resizeWithWindowRef.current = resizeActive;
        resizeCoordinatorRef.current?.reset(
          result.attached.session.terminal_size,
        );
        if (resizeActive) {
          resizeCoordinatorRef.current?.setDesired(requestedTerminalSize);
          resizeCoordinatorRef.current?.setEnabled(true);
        }
        publishShellState(result.attached.shell_state);
        setState((current) => ({
          ...current,
          phase: "attached",
          attachment_id: result.attached.attachment_id,
          session: result.attached.session,
          input_lease: result.attached.input_lease,
          layout_lease: result.attached.layout_lease,
          terminal_size_mismatch: result.attached.terminal_size_mismatch,
          history_gap: result.attached.history_gap,
          reconnect_sequence: null,
          resize_with_window: resizeActive,
          message:
            resizeWithWindow && !resizeActive
              ? "Another client controls this session's terminal size."
              : null,
        }));
        responseReady = true;
        for (const event of pendingEvents) {
          queueEvent(event, generation);
        }
        renderer.focus();
      } catch (error) {
        if (generation === generationRef.current) {
          setFailure(error);
        }
      }
    },
    [publishShellState, queueEvent, renderer, setFailure],
  );

  const connectAt = useCallback(
    (
      session: SessionSummary,
      resumeFrom: string | null,
      resizeWithWindow: boolean,
    ): Promise<void> => {
      if (!renderer) {
        setFailure("The terminal renderer is not ready.");
        return Promise.resolve();
      }

      const generation = generationRef.current + 1;
      generationRef.current = generation;
      inputLeaseOwnedRef.current = false;
      layoutLeaseOwnedRef.current = false;
      resizeWithWindowRef.current = resizeWithWindow;
      inputPumpRef.current?.clear();
      layoutLeasePumpRef.current?.reset();
      resizeCoordinatorRef.current?.reset(session.terminal_size);
      pendingShellStateRef.current = null;
      appliedSequenceRef.current = resumeFrom;
      setState({
        ...INITIAL_STATE,
        phase: resumeFrom ? "reconnecting" : "connecting",
        session,
        applied_sequence: resumeFrom,
        resize_with_window: resizeWithWindow,
      });

      return connectionQueueRef.current!.submit(
        () =>
          performConnection({
            generation,
            session,
            resumeFrom,
            resizeWithWindow,
          }),
        (error) => {
          if (generation === generationRef.current) {
            setFailure(error);
          }
        },
      );
    },
    [performConnection, renderer, setFailure],
  );

  const connect = useCallback(
    async (session: SessionSummary, options: ConnectOptions = {}) =>
      connectAt(session, null, options.resize_with_window ?? false),
    [connectAt],
  );

  const reconnect = useCallback(async () => {
    const current = stateRef.current;
    if (current.session) {
      await connectAt(
        current.session,
        current.reconnect_sequence,
        current.resize_with_window,
      );
    }
  }, [connectAt]);

  const detach = useCallback(async () => {
    const generation = generationRef.current + 1;
    generationRef.current = generation;
    inputLeaseOwnedRef.current = false;
    layoutLeaseOwnedRef.current = false;
    resizeWithWindowRef.current = false;
    inputPumpRef.current?.clear();
    layoutLeasePumpRef.current?.reset();
    resizeCoordinatorRef.current?.reset();
    connectionQueueRef.current?.cancelPending();
    const attachmentId = activeAttachmentRef.current;
    activeAttachmentRef.current = null;
    channelRef.current = null;
    if (attachmentId) {
      try {
        await detachAttachment({ attachment_id: attachmentId });
      } catch (error) {
        if (generation === generationRef.current) {
          setFailure(error);
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
  }, [setFailure]);

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
    connect,
    reconnect,
    detach,
    handleInput,
    toggleInputLease,
    toggleResizeWithWindow,
  };
}
