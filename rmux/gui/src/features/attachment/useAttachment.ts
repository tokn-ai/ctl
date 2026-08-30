import { useCallback, useEffect, useRef, useState } from "react";
import type { Channel } from "@tauri-apps/api/core";
import { decodeBase64, encodeBase64, sequenceAtLeast } from "../../lib/bytes";
import {
  acknowledgeAttachmentEvent,
  acquireAttachmentLease,
  detachAttachment,
  errorMessage,
  openAttachment,
  releaseAttachmentLease,
  resizeAttachment,
  sendInput,
} from "../../lib/tauri";
import type {
  AttachmentEvent,
  AttachmentViewState,
  LeaseKind,
  SessionSummary,
  ShellStateSummary,
  TerminalSize,
} from "../../lib/types";
import type { XtermRenderer } from "../terminal/XtermRenderer";
import { InputPump } from "./InputPump";
import { LatestTaskQueue } from "./LatestTaskQueue";

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

interface ConnectionRequest {
  generation: number;
  session: SessionSummary;
  resumeFrom: string | null;
}

export interface AttachmentActions {
  state: AttachmentViewState;
  connect(session: SessionSummary): Promise<void>;
  reconnect(): Promise<void>;
  detach(): Promise<void>;
  handleInput(data: Uint8Array): void;
  toggleInputLease(): Promise<void>;
  useWindowForLayout(): Promise<void>;
  releaseLayout(): Promise<void>;
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
  const pendingLayoutSizeRef = useRef<TerminalSize | null>(null);
  const inputLeaseOwnedRef = useRef(false);
  const connectionQueueRef = useRef<LatestTaskQueue | null>(null);
  if (!connectionQueueRef.current) {
    connectionQueueRef.current = new LatestTaskQueue();
  }

  useEffect(() => {
    stateRef.current = state;
    inputLeaseOwnedRef.current = state.input_lease.owned_by_client;
  }, [state]);

  const setFailure = useCallback((error: unknown) => {
    setState((current) => ({
      ...current,
      phase: "error",
      message: errorMessage(error),
    }));
  }, []);

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
      inputPumpRef.current?.clear();
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
          setState((current) => ({
            ...current,
            session: current.session
              ? { ...current.session, terminal_size: event.terminal_size }
              : current.session,
          }));
          break;
        case "lease_status":
          if (event.lease === "input") {
            inputLeaseOwnedRef.current = event.status.owned_by_client;
          }
          setState((current) => ({
            ...current,
            input_lease:
              event.lease === "input" ? event.status : current.input_lease,
            layout_lease:
              event.lease === "layout" ? event.status : current.layout_lease,
          }));
          if (
            event.lease === "layout" &&
            event.status.owned_by_client &&
            pendingLayoutSizeRef.current
          ) {
            const requestedSize = pendingLayoutSizeRef.current;
            pendingLayoutSizeRef.current = null;
            await resizeAttachment({
              attachment_id: event.attachment_id,
              terminal_size: requestedSize,
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
          setState((current) => ({
            ...current,
            phase: "ended",
            message:
              event.exit_code === null
                ? "Session ended."
                : `Session ended with exit code ${event.exit_code}.`,
          }));
          break;
        case "attachment_exited":
          activeAttachmentRef.current = null;
          channelRef.current = null;
          inputLeaseOwnedRef.current = false;
          inputPumpRef.current?.clear();
          setState((current) => ({
            ...current,
            attachment_id: null,
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
          inputPumpRef.current?.clear();
          setState((current) => ({
            ...current,
            attachment_id: null,
            phase: "error",
            reconnect_sequence: null,
            message: event.message,
          }));
          break;
      }
    },
    [acknowledge, publishAppliedSequence, publishShellState, renderer],
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
      const { generation, session, resumeFrom } = request;
      if (generation !== generationRef.current || !renderer) {
        return;
      }

      const previousAttachment = activeAttachmentRef.current;
      activeAttachmentRef.current = null;
      channelRef.current = null;

      if (previousAttachment) {
        try {
          await detachAttachment({ attachment_id: previousAttachment });
        } catch {
          // Opening a replacement attachment is still safe: the backend fences
          // every command with its attachment id and detaches the old actor.
        }
      }
      if (generation !== generationRef.current) {
        return;
      }

      const proposed = renderer.proposeDimensions();
      const pendingEvents: AttachmentEvent[] = [];
      let responseReady = false;
      try {
        const result = await openAttachment(
          {
            session: session.session_id,
            resume_from: resumeFrom,
            terminal_size: proposed
              ? terminalSize(proposed.columns, proposed.rows)
              : terminalSize(80, 24),
            request_input_lease: true,
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
          message: null,
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
    (session: SessionSummary, resumeFrom: string | null): Promise<void> => {
      if (!renderer) {
        setFailure("The terminal renderer is not ready.");
        return Promise.resolve();
      }

      const generation = generationRef.current + 1;
      generationRef.current = generation;
      inputLeaseOwnedRef.current = false;
      inputPumpRef.current?.clear();
      pendingShellStateRef.current = null;
      pendingLayoutSizeRef.current = null;
      appliedSequenceRef.current = resumeFrom;
      setState({
        ...INITIAL_STATE,
        phase: resumeFrom ? "reconnecting" : "connecting",
        session,
        applied_sequence: resumeFrom,
      });

      return connectionQueueRef.current!.submit(
        () => performConnection({ generation, session, resumeFrom }),
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
    async (session: SessionSummary) => connectAt(session, null),
    [connectAt],
  );

  const reconnect = useCallback(async () => {
    const current = stateRef.current;
    if (current.session) {
      await connectAt(current.session, current.reconnect_sequence);
    }
  }, [connectAt]);

  const detach = useCallback(async () => {
    const generation = generationRef.current + 1;
    generationRef.current = generation;
    inputLeaseOwnedRef.current = false;
    inputPumpRef.current?.clear();
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

  const useWindowForLayout = useCallback(async () => {
    const attachmentId = activeAttachmentRef.current;
    const generation = generationRef.current;
    const proposed = renderer?.proposeDimensions();
    if (!attachmentId || !proposed) {
      setState((current) => ({
        ...current,
        message: "The terminal viewport is not ready for layout measurement.",
      }));
      return;
    }
    const requestedSize = terminalSize(proposed.columns, proposed.rows);
    try {
      if (stateRef.current.layout_lease.owned_by_client) {
        await resizeAttachment({
          attachment_id: attachmentId,
          terminal_size: requestedSize,
        });
      } else {
        pendingLayoutSizeRef.current = requestedSize;
        await acquireAttachmentLease({
          attachment_id: attachmentId,
          lease: "layout",
        });
      }
    } catch (error) {
      pendingLayoutSizeRef.current = null;
      if (
        generation === generationRef.current &&
        attachmentId === activeAttachmentRef.current
      ) {
        setFailure(error);
      }
    }
  }, [renderer, setFailure]);

  const releaseLayout = useCallback(
    async () => changeLease("layout", false),
    [changeLease],
  );

  return {
    state,
    connect,
    reconnect,
    detach,
    handleInput,
    toggleInputLease,
    useWindowForLayout,
    releaseLayout,
  };
}
