// @vitest-environment jsdom
import { act, cleanup, render, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import type { Terminal as HeadlessTerminal } from "@xterm/headless";
import type {
  AttachmentEvent,
  CheckpointEvent,
  OpenAttachmentRequest,
  SessionSummary,
} from "../../lib/types";
import { sessionKey } from "../targets/targets";
import { XtermRenderer } from "../terminal/XtermRenderer";
import { useAttachment } from "./useAttachment";
import { useSessionAttachments } from "./useSessionAttachments";
import { reconnectComponentAttachments, resetComponentAttachments } from "./componentActions";
import { NotificationProvider } from "../notifications/NotificationContext";
import { NotificationStore } from "../notifications/NotificationStore";
import { useWorkbenchNotifications } from "../notifications/useWorkbenchNotifications";
import { ManualReconnectProvider, type ManualReconnectRequest } from "./ManualReconnect";

const xterm = vi.hoisted(() => ({
  instances: [] as {
    terminal: HeadlessTerminal;
    container: HTMLElement;
    dispose: ReturnType<typeof vi.fn>;
  }[],
}));

// Exercise the real presenter/parser and hook together, replacing only the
// browser canvas and native transport that aren't available in jsdom.
vi.mock("@xterm/xterm", async () => {
  const { Terminal } = await import("@xterm/headless");
  return {
    Terminal: class {
      constructor(options: ConstructorParameters<typeof Terminal>[0]) {
        const terminal = new Terminal({ ...options, allowProposedApi: true });
        const dispose = vi.fn(() => terminal.dispose());
        return {
          write: (data: Uint8Array, callback: () => void) => terminal.write(data, callback),
          resize: (columns: number, rows: number) => terminal.resize(columns, rows),
          dispose,
          open: (container: HTMLElement) => {
            container.append(document.createElement("div"));
            xterm.instances.push({ terminal, container, dispose });
          },
          loadAddon: () => undefined,
          onData: () => undefined,
          onBinary: () => undefined,
          focus: () => undefined,
        };
      }
    },
  };
});
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    proposeDimensions() { return { cols: 80, rows: 24 }; }
  },
}));

const api = vi.hoisted(() => ({
  openAttachment: vi.fn(),
  acknowledgeAttachmentEvent: vi.fn(),
  detachAttachment: vi.fn(),
  acquireAttachmentLease: vi.fn(),
  releaseAttachmentLease: vi.fn(),
  resizeAttachment: vi.fn(),
  sendInput: vi.fn(),
  sessionCache: vi.fn(),
  sshConnectionStatus: vi.fn(),
}));
vi.mock("../../lib/tauri", () => api);

const size = { columns: 80, rows: 24, pixel_width: null, pixel_height: null };
const first: SessionSummary = {
  target: { kind: "local" },
  session_id: "first",
  terminal_id: "first-terminal",
  view_id: "first-view",
  name: "first",
  status: "running",
  next_sequence: "0",
  terminal_size: size,
};
const second: SessionSummary = { ...first, session_id: "second", terminal_id: "second-terminal", view_id: "second-view", name: "second" };
const sessions = [first, second];
let renderer: XtermRenderer;
const pane_renderers: XtermRenderer[] = [];
let container: HTMLElement;
let channels: Map<string, (event: AttachmentEvent) => void>;

function checkpoint(attachment_id: string, text: string, sequence = "0"): CheckpointEvent {
  return {
    event_type: "checkpoint",
    attachment_id,
    event_id: `checkpoint-${attachment_id}`,
    checkpoint: {
      format: "vt",
      format_version: 1,
      terminal_size: size,
      sequence,
      payload_base64: btoa(text),
      input_prefix_base64: "",
    },
    history: {
      format: "lines",
      format_version: 1,
      sequence,
      generation: "0",
      revision: "0",
      retained_bytes: "0",
      truncated: false,
      lines: [],
    },
    history_gap: false,
  };
}

function visibleTerminal() {
  return [...xterm.instances].reverse().find((instance) =>
    instance.container.isConnected && !instance.container.hidden,
  )!;
}

function line(terminal: HeadlessTerminal, row = 0) {
  return terminal.buffer.active.getLine(row)?.translateToString(true);
}

async function emit(event: AttachmentEvent) {
  await act(async () => { channels.get(event.attachment_id)!(event); });
  if ("event_id" in event) {
    await waitFor(() => expect(api.acknowledgeAttachmentEvent).toHaveBeenCalledWith({
      attachment_id: event.attachment_id,
      event_id: event.event_id,
    }));
  }
}

beforeEach(() => {
  vi.resetAllMocks();
  xterm.instances.length = 0;
  channels = new Map();
  container = document.createElement("div");
  document.body.append(container);
  renderer = new XtermRenderer(container, () => undefined, size);
  api.sessionCache.mockResolvedValue({ kind: "loaded", cache: null });
  api.detachAttachment.mockResolvedValue(undefined);
  api.acknowledgeAttachmentEvent.mockResolvedValue(undefined);
  api.openAttachment.mockImplementation(async (
    request: OpenAttachmentRequest,
    on_event: (event: AttachmentEvent) => void,
  ) => {
    const attachment_id = `attachment-${channels.size}`;
    channels.set(attachment_id, on_event);
    return {
      attached: {
        attachment_id,
        session: sessions.find((session) => session.session_id === request.session || session.terminal_id === request.session),
        replay_from: request.resume_from ?? "0",
        history_gap: false,
        terminal_size_mismatch: false,
        input_lease: { held: true, owned_by_client: true },
        layout_lease: { held: false, owned_by_client: false },
        shell_state: {
          shell_type: "unknown",
          cwd: null,
          running_command: null,
          prompt_phase: "unknown",
          tui_hint: "unknown",
          revision: "0",
          observed_sequence: "0",
        },
      },
      channel: {},
    };
  });
});

afterEach(async () => {
  cleanup();
  vi.useRealTimers();
  for (const pane_renderer of pane_renderers.splice(0)) pane_renderer.dispose();
  renderer.dispose();
  container.remove();
  await Promise.resolve();
});

describe("explicit SSH reconnect preparation", () => {
  const remote: SessionSummary = {
    ...first,
    target: { kind: "ssh", host_id: "fixture", method_id: "runtime", destination: "runtime-route" },
  };
  const authentication_required = { code: "ssh_authentication_required", message: "Authentication required" };

  function setup(request: ManualReconnectRequest) {
    return renderHook(() => useAttachment(renderer), {
      wrapper: ({ children }: { children: ReactNode }) =>
        <ManualReconnectProvider request={request}>{children}</ManualReconnectProvider>,
    });
  }

  beforeEach(() => {
    const open = api.openAttachment.getMockImplementation()!;
    api.openAttachment.mockImplementation(async (request: OpenAttachmentRequest, ...args: unknown[]) => {
      const response = await open(request, ...args);
      return { ...response, attached: { ...response.attached, session: { ...response.attached.session, target: request.target } } };
    });
  });

  it("coalesces clicks and preserves the exact pane, screen and cursor through host preparation", async () => {
    let finish!: (connected: boolean) => void;
    const prepare = vi.fn<ManualReconnectRequest>(() => new Promise((resolve) => { finish = resolve; }));
    const { result } = setup(prepare);
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "retained screen", "15"));
    const visible = visibleTerminal();
    vi.useFakeTimers();
    const attachment_id = result.current.state.attachment_id!;
    await act(async () => channels.get(attachment_id)!({
      event_type: "attachment_exited", attachment_id, reason: "connection_closed",
      exit_code: null, next_sequence: "15", received_sequence: "15",
    }));
    let reconnecting!: Promise<void>;
    await act(async () => {
      reconnecting = result.current.reconnect();
      expect(result.current.reconnect()).toBe(reconnecting);
    });
    expect(prepare).toHaveBeenCalledExactlyOnceWith(remote.target, expect.any(AbortSignal));
    expect(api.openAttachment).toHaveBeenCalledOnce();
    expect(api.detachAttachment).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(api.openAttachment).toHaveBeenCalledOnce();
    await act(async () => { finish(true); await reconnecting; });
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({
      target: remote.target, session: remote.terminal_id, resume_from: "15", request_layout_lease: false,
    });
    expect(visibleTerminal()).toBe(visible);
    expect(line(visible.terminal)).toBe("retained screen");
    expect(result.current.state.phase).toBe("attached");
  });

  it("retains the failed session and cursor when host authentication is cancelled", async () => {
    const prepare = vi.fn<ManualReconnectRequest>().mockResolvedValue(false);
    const { result } = setup(prepare);
    api.openAttachment.mockRejectedValueOnce(authentication_required);
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    const before = result.current.state;
    await act(async () => { await result.current.reconnect(); });
    expect(result.current.state).toMatchObject({
      phase: "error", error_code: "attachment_cancelled", session: before.session,
      reconnect_sequence: before.reconnect_sequence,
    });
    expect(api.openAttachment).toHaveBeenCalledOnce();
  });

  it("does not restart automatic recovery after the user cancels host preparation", async () => {
    const prepare = vi.fn<ManualReconnectRequest>().mockResolvedValue(false);
    const { result } = setup(prepare);
    vi.useFakeTimers();
    api.openAttachment.mockRejectedValueOnce({ code: "remote_connection_timeout", message: "Service timed out" });
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    expect(result.current.state.phase).toBe("retry_wait");
    await act(async () => { await result.current.reconnect(); });
    expect(result.current.state).toMatchObject({ phase: "error", error_code: "attachment_cancelled", session: remote });
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(api.openAttachment).toHaveBeenCalledOnce();
    expect(prepare).toHaveBeenCalledOnce();
  });

  it.each(["new connection", "detach", "cancel", "restart", "unmount"])(
    "aborts preparation on %s and rejects its late success", async (operation) => {
      let finish!: (connected: boolean) => void;
      const prepare = vi.fn<ManualReconnectRequest>(() => new Promise((resolve) => { finish = resolve; }));
      const { result, unmount } = setup(prepare);
      api.openAttachment.mockRejectedValueOnce(authentication_required);
      await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
      let reconnecting!: Promise<void>;
      await act(async () => { reconnecting = result.current.reconnect(); });
      const signal = prepare.mock.calls[0][1];
      await act(async () => {
        if (operation === "new connection") await result.current.connect(second);
        else if (operation === "detach") await result.current.detach();
        else if (operation === "cancel") result.current.cancelPendingConnection(remote);
        else if (operation === "restart") result.current.resetAfterDaemonRestart();
        else unmount();
      });
      expect(signal.aborted).toBe(true);
      await act(async () => { finish(true); await reconnecting; });
      expect(api.openAttachment).toHaveBeenCalledTimes(operation === "new connection" ? 2 : 1);
      if (operation === "new connection") expect(result.current.state.session).toEqual({ ...second, terminal_size_known: true });
    },
  );

  it.each(["ssh_authentication_required", "ssh_host_disconnected"])(
    "retries host preparation once when the checked master changes (%s)", async (code) => {
      const prepare = vi.fn<ManualReconnectRequest>().mockResolvedValue(true);
      const { result } = setup(prepare);
      api.openAttachment.mockRejectedValueOnce(authentication_required);
      await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
      api.openAttachment.mockRejectedValueOnce({ code, message: "Master changed after status check" });
      await act(async () => { await result.current.reconnect(); });
      expect(prepare.mock.calls.map(([, , force]) => force)).toEqual([undefined, true]);
      expect(api.openAttachment).toHaveBeenCalledTimes(3);
      expect(result.current.state.phase).toBe("attached");
      expect(result.current.state.session?.target).toEqual(remote.target);
    },
  );

  it("stops after one forced host retry if authentication still cannot be used", async () => {
    const prepare = vi.fn<ManualReconnectRequest>().mockResolvedValue(true);
    const { result } = setup(prepare);
    api.openAttachment.mockRejectedValue(authentication_required);
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    await act(async () => { await result.current.reconnect(); });
    expect(prepare.mock.calls.map(([, , force]) => force)).toEqual([undefined, true]);
    expect(api.openAttachment).toHaveBeenCalledTimes(3);
    expect(result.current.state).toMatchObject({ phase: "error", error_code: authentication_required.code });
  });

  it("does not authenticate for an old native failure after a different session is selected", async () => {
    const prepare = vi.fn<ManualReconnectRequest>().mockResolvedValue(true);
    const { result } = setup(prepare);
    api.openAttachment.mockRejectedValueOnce(authentication_required);
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    api.openAttachment.mockImplementationOnce((_request, _on_event, signal: AbortSignal) =>
      new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(authentication_required), { once: true });
      }));
    let reconnecting!: Promise<void>;
    await act(async () => { reconnecting = result.current.reconnect(); });
    expect(api.openAttachment).toHaveBeenCalledTimes(2);
    await act(async () => { await result.current.connect(second); await reconnecting; });
    expect(result.current.state.session).toEqual({ ...second, terminal_size_known: true });
    expect(result.current.state.phase).toBe("attached");
    expect(prepare).toHaveBeenCalledOnce();
  });

  it("preserves a live actor and renderer when its preflight status check fails", async () => {
    const prepare = vi.fn<ManualReconnectRequest>().mockRejectedValue({ code: "ssh_status_timeout", message: "Status unknown" });
    const { result } = setup(prepare);
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "live screen", "11"));
    const actor = result.current.state.attachment_id;
    await act(async () => { await result.current.reconnect(); });
    expect(result.current.state).toMatchObject({ phase: "attached", attachment_id: actor, message: "Status unknown" });
    expect(renderer.resumeSequence()).toBe("11");
    expect(line(visibleTerminal().terminal)).toBe("live screen");
    expect(api.detachAttachment).not.toHaveBeenCalled();
    act(() => result.current.handleInput(new Uint8Array([65])));
    await waitFor(() => expect(api.sendInput).toHaveBeenCalledWith({ attachment_id: actor, data_base64: "QQ==" }));
  });

  it("resumes automatic backoff after deferred cleanup without forcing host authentication", async () => {
    const prepare = vi.fn<ManualReconnectRequest>().mockResolvedValue(true);
    const { result } = setup(prepare);
    api.openAttachment.mockRejectedValueOnce(authentication_required);
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    vi.useFakeTimers();
    let finish_cleanup!: () => void;
    api.detachAttachment.mockImplementationOnce(() => new Promise<void>((resolve) => { finish_cleanup = resolve; }));
    vi.spyOn(renderer, "recreate").mockRejectedValueOnce({ code: "remote_connection_timeout", message: "Synthetic service timeout" });
    let reconnecting!: Promise<void>;
    await act(async () => { reconnecting = result.current.reconnect(); });
    expect(result.current.state).toMatchObject({ phase: "error", error_code: "remote_connection_timeout" });
    expect(api.detachAttachment).toHaveBeenCalledOnce();
    await act(async () => { finish_cleanup(); await reconnecting; });
    expect(result.current.state.phase).toBe("retry_wait");
    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    expect(api.openAttachment).toHaveBeenCalledTimes(3);
    expect(result.current.state.phase).toBe("attached");
    expect(prepare).toHaveBeenCalledOnce();
  });

  it("keeps automatic and component reconnects outside interactive host preparation", async () => {
    const prepare = vi.fn<ManualReconnectRequest>().mockResolvedValue(true);
    const { result } = setup(prepare);
    await act(async () => { await result.current.connect(remote, { terminal_id: remote.terminal_id }); });
    vi.useFakeTimers();
    const first_actor = result.current.state.attachment_id!;
    await act(async () => channels.get(first_actor)!({
      event_type: "attachment_exited", attachment_id: first_actor, reason: "connection_closed",
      exit_code: null, next_sequence: null, received_sequence: "0",
    }));
    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    const second_actor = result.current.state.attachment_id!;
    await act(async () => {
      const [reconnected] = await reconnectComponentAttachments([second_actor]);
      expect(reconnected.replacement_attachment_id).not.toBeNull();
    });
    expect(api.openAttachment).toHaveBeenCalledTimes(3);
    expect(prepare).not.toHaveBeenCalled();
  });
});

describe("connection evidence and transitions", () => {
  it("keeps confirmed exit sticky against events already queued by the same actor", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    const attachment_id = result.current.state.attachment_id!;
    await act(async () => {
      const send = channels.get(attachment_id)!;
      send({ event_type: "session_ended", attachment_id, session_id: first.session_id, exit_code: 7 });
      send({ event_type: "attachment_error", attachment_id, code: "backend_error", message: "Pipe closed" });
      send({ event_type: "attachment_exited", attachment_id, reason: "connection_closed", exit_code: null, next_sequence: null, received_sequence: "0" });
    });
    expect(result.current.state).toMatchObject({ phase: "ended", message: "Session ended with exit code 7.", attachment_id: null });
    expect(api.openAttachment).toHaveBeenCalledOnce();
    expect(result.current.state.retry_at_ms).toBeNull();
  });

  it("revokes input and fences late events when a live attachment command fails", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    const attachment_id = result.current.state.attachment_id!;
    api.releaseAttachmentLease.mockRejectedValueOnce({ code: "protocol_version_mismatch", message: "Update required" });
    await act(async () => { await result.current.toggleInputLease(); });
    expect(result.current.state).toMatchObject({ phase: "error", attachment_id: null, input_lease: { owned_by_client: false } });
    expect(api.detachAttachment).toHaveBeenCalledWith({ attachment_id });
    act(() => result.current.handleInput(new Uint8Array([65])));
    expect(api.sendInput).not.toHaveBeenCalled();
    await act(async () => channels.get(attachment_id)!({ event_type: "session_ended", attachment_id, session_id: first.session_id, exit_code: 0 }));
    expect(result.current.state.phase).toBe("error");
  });

  it("records retry wait separately and reconnects even when there is no replay cursor", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    vi.useFakeTimers();
    const attachment_id = result.current.state.attachment_id!;
    api.openAttachment.mockImplementationOnce(() => new Promise(() => {}));
    await act(async () => channels.get(attachment_id)!({ event_type: "attachment_exited", attachment_id, reason: "connection_closed", exit_code: null, next_sequence: null, received_sequence: "0" }));
    expect(result.current.state).toMatchObject({ phase: "retry_wait", attachment_id: null, retry_at_ms: Date.now() + 250 });
    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    expect(api.openAttachment).toHaveBeenCalledTimes(2);
    expect(result.current.state).toMatchObject({ phase: "reconnecting", retry_at_ms: null });
  });

  it("releases a successful native open if the renderer cannot adopt it", async () => {
    vi.spyOn(renderer, "recreate").mockRejectedValueOnce({ code: "local_cache_failed", message: "Renderer unavailable" });
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    expect(result.current.state).toMatchObject({ phase: "error", attachment_id: null });
    expect(api.detachAttachment).toHaveBeenCalledWith({ attachment_id: "attachment-0" });
  });

  it("counts healthy time across resizes before resetting the retry budget", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    vi.spyOn(renderer, "recreate").mockResolvedValue(undefined);
    vi.spyOn(renderer, "resize").mockResolvedValue(undefined);
    vi.useFakeTimers();
    const close = async () => {
      const attachment_id = result.current.state.attachment_id!;
      await act(async () => channels.get(attachment_id)!({ event_type: "attachment_exited", attachment_id, reason: "connection_closed", exit_code: null, next_sequence: null, received_sequence: "0" }));
    };
    await close();
    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    expect(result.current.state.phase).toBe("attached");
    await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });
    const attachment_id = result.current.state.attachment_id!;
    await act(async () => channels.get(attachment_id)!({ event_type: "pty_geometry_changed", attachment_id, event_id: "resize", observed_sequence: "0", terminal_size: { ...size, columns: 100 } }));
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    await close();
    expect(result.current.state).toMatchObject({ phase: "retry_wait", retry_at_ms: Date.now() + 250 });
  });
});

function renderPaneAttachment() {
  const pane_renderer = new XtermRenderer(document.createElement("div"), () => undefined, size);
  pane_renderers.push(pane_renderer);
  return renderHook(() => useAttachment(pane_renderer));
}

describe("pending remote attachments", () => {
  it("reconnects a native-selected attachment with a real replacement ID while preserving other panes", async () => {
    const root = renderHook(() => useAttachment(renderer));
    const pane = renderPaneAttachment();
    await act(async () => { await root.result.current.connect(first); await pane.result.current.connect(second); });
    const selected = root.result.current.state.attachment_id!;
    const other = pane.result.current.state.attachment_id;
    let results!: Awaited<ReturnType<typeof reconnectComponentAttachments>>;
    await act(async () => { results = await reconnectComponentAttachments([selected]); });
    expect(results).toEqual([{ attachment_id: selected, replacement_attachment_id: root.result.current.state.attachment_id, error: null }]);
    expect(root.result.current.state.attachment_id).not.toBe(selected);
    expect(api.detachAttachment).toHaveBeenCalledWith({ attachment_id: selected });
    expect(pane.result.current.state.attachment_id).toBe(other);
    expect(api.detachAttachment).not.toHaveBeenCalledWith({ attachment_id: other });
  });

  it("invalidates mounted root and pane owners after a committed daemon reset", async () => {
    const root = renderHook(() => useAttachment(renderer));
    const pane = renderPaneAttachment();
    await act(async () => { await root.result.current.connect(first); await pane.result.current.connect(second); });
    act(() => { resetComponentAttachments({ scope: "local", host_ids: [], session_ids: [], attachment_ids: [] }); });
    expect(root.result.current.state.phase).toBe("idle");
    expect(pane.result.current.state.phase).toBe("idle");
    expect(root.result.current.state.session).toBeNull();
    expect(pane.result.current.state.session).toBeNull();
  });

  it("sizes the view without treating an individual pane geometry event as a canvas resize", async () => {
    const open = api.openAttachment.getMockImplementation()!;
    api.openAttachment.mockImplementation(async (...args) => {
      const result = await open(...args);
      result.attached.layout_lease = { held: true, owned_by_client: true };
      return result;
    });
    let measure!: (size: { columns: number; rows: number }) => void;
    vi.spyOn(renderer, "observeDimensions").mockImplementation((callback) => { measure = callback; return () => {}; });
    const { result } = renderHook(() => useAttachment(renderer, true));
    await act(async () => { await result.current.connect(first); });
    expect(api.openAttachment.mock.lastCall?.[0].request_layout_lease).toBe(true);
    act(() => measure({ columns: 100, rows: 40 }));
    await waitFor(() => expect(api.resizeAttachment).toHaveBeenCalledExactlyOnceWith({ attachment_id: "attachment-0", terminal_size: { ...size, columns: 100, rows: 40 } }));
    await emit({ event_type: "pty_geometry_changed", attachment_id: "attachment-0", event_id: "pane-resized", terminal_size: { ...size, columns: 50, rows: 40 }, observed_sequence: "0" });
    act(() => measure({ columns: 100, rows: 40 }));
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 100)); });
    expect(api.resizeAttachment).toHaveBeenCalledTimes(1);
    expect(result.current.state.session?.terminal_size.columns).toBe(50);
  });

  it("releases only this renderer's attachment before opening its replacement", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    const previous_id = result.current.state.attachment_id;
    let finish!: () => void;
    api.detachAttachment.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    let switching!: Promise<void>;
    await act(async () => { switching = result.current.connect(second); });
    expect(api.detachAttachment).toHaveBeenCalledExactlyOnceWith({ attachment_id: previous_id });
    expect(api.openAttachment).toHaveBeenCalledTimes(1);
    await act(async () => { finish(); await switching; });
    expect(api.openAttachment).toHaveBeenCalledTimes(2);
    expect(result.current.state.session?.session_id).toBe(second.session_id);
  });

  const remote: SessionSummary = {
    ...first,
    target: { kind: "ssh", destination: "offline-host" },
  };

  function stallNextOpen() {
    const aborted = vi.fn();
    api.openAttachment.mockImplementationOnce((_request, _on_event, signal: AbortSignal) =>
      new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => {
          aborted();
          reject({ code: "attachment_cancelled", message: "Cancelled" });
        }, { once: true });
      }),
    );
    return aborted;
  }

  it("switches to a local session without waiting for the stalled SSH connection", async () => {
    const aborted = stallNextOpen();
    const { result } = renderHook(() => useAttachment(renderer));
    let opening!: Promise<void>;
    await act(async () => { opening = result.current.connect(remote); });
    expect(result.current.state.phase).toBe("connecting");

    await act(async () => {
      await result.current.connect(second);
      await opening;
    });
    expect(aborted).toHaveBeenCalledOnce();
    expect(result.current.state.phase).toBe("attached");
    expect(result.current.state.session).toEqual({ ...second, terminal_size_known: true });
    expect(result.current.state.error_code).toBeNull();
    await act(async () => { result.current.handleInput(new TextEncoder().encode("pwd\r")); });
    await waitFor(() => expect(api.sendInput).toHaveBeenCalled());
  });

  it.each(["disconnect", "forget", "restart"])("cancels a pending open on %s", async (action) => {
    const aborted = stallNextOpen();
    const { result } = renderHook(() => useAttachment(renderer));
    let opening!: Promise<void>;
    await act(async () => { opening = result.current.connect(remote); });
    await act(async () => {
      if (action === "disconnect") await result.current.detach();
      else if (action === "forget") result.current.cancelPendingConnection(remote);
      else result.current.resetAfterDaemonRestart();
      await opening;
    });
    expect(aborted).toHaveBeenCalledOnce();
    expect(result.current.state.phase).toBe("idle");
    expect(result.current.state.session).toBeNull();
    expect(result.current.state.error_code).toBeNull();
  });

  it("cancels the pending native open when the hook unmounts", async () => {
    const aborted = stallNextOpen();
    const { result, unmount } = renderHook(() => useAttachment(renderer));
    let opening!: Promise<void>;
    await act(async () => { opening = result.current.connect(remote); });
    unmount();
    await opening;
    expect(aborted).toHaveBeenCalledOnce();
  });
});

describe("opened session cache", () => {
  it.each(["0", "5"])("preserves a root attachment's screen and history across network reconnect at sequence %s", async (sequence) => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    const initial = checkpoint(result.current.state.attachment_id!, "\u001b[2J\u001b[Hold screen", sequence);
    initial.history.lines = ["previous history"];
    await emit(initial);
    const saved = visibleTerminal();
    expect(line(saved.terminal, 0)).toBe("previous history");
    expect(line(saved.terminal, saved.terminal.buffer.active.baseY)).toBe("old screen");

    await emit({ event_type: "attachment_exited", attachment_id: initial.attachment_id, reason: "connection_closed", exit_code: null, next_sequence: sequence, received_sequence: sequence });
    await waitFor(() => expect(api.openAttachment).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.state.phase).toBe("attached"));
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: first.terminal_id, resume_from: sequence });
    expect(visibleTerminal() === saved).toBe(true);
    expect(saved.dispose).not.toHaveBeenCalled();
    await emit({ event_type: "output", attachment_id: result.current.state.attachment_id!, event_id: "resumed-output", sequence_start: sequence, sequence_end: "10", data_base64: btoa(" and new output") });
    expect(line(saved.terminal, 0)).toBe("previous history");
    expect(line(saved.terminal, saved.terminal.buffer.active.baseY)).toBe("old screen and new output");
  });

  it("preserves an alternate screen and its cursor-positioned updates on network reconnect", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    const initial = checkpoint(result.current.state.attachment_id!, "\u001b[2J\u001b[Hshell screen\u001b[?1049h\u001b[Hscreen header\u001b[10;20Hold", "5");
    initial.history.lines = ["previous history"];
    await emit(initial);
    const saved = visibleTerminal();
    expect(saved.terminal.buffer.active.type).toBe("alternate");
    await emit({ event_type: "attachment_exited", attachment_id: initial.attachment_id, reason: "connection_closed", exit_code: null, next_sequence: "5", received_sequence: "5" });
    await waitFor(() => expect(api.openAttachment).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.state.phase).toBe("attached"));
    await emit({ event_type: "output", attachment_id: result.current.state.attachment_id!, event_id: "tui-resumed", sequence_start: "5", sequence_end: "10", data_base64: btoa("\u001b[10;20Hnew") });
    expect(visibleTerminal() === saved).toBe(true);
    expect(saved.terminal.buffer.active.type).toBe("alternate");
    expect(line(saved.terminal)).toBe("screen header");
    expect(line(saved.terminal, 9)).toBe(`${" ".repeat(19)}new`);
    expect(saved.terminal.buffer.normal.getLine(0)?.translateToString(true)).toBe("previous history");
    expect(saved.dispose).not.toHaveBeenCalled();
  });

  it.each(["invalidated", "replaced", "ahead"])("requests a checkpoint when the retained renderer cursor was %s", async (change) => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    const attachment_id = result.current.state.attachment_id!;
    await emit(checkpoint(attachment_id, "old screen", "5"));
    await emit({ event_type: "attachment_exited", attachment_id, reason: "connection_closed", exit_code: null, next_sequence: "5", received_sequence: "5" });
    if (change === "invalidated") renderer.invalidateResumeSequence();
    else if (change === "replaced") renderer.retainSessions(new Set());
    else await renderer.write(new TextEncoder().encode(" unacknowledged"), "7");

    await act(async () => { await result.current.reconnect(); });
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: first.terminal_id, resume_from: null });
    await emit(checkpoint(result.current.state.attachment_id!, "authoritative screen", "9"));
    expect(line(visibleTerminal().terminal)).toBe("authoritative screen");
  });

  it.each(["moved pane", "lost cache"])("retries once with a checkpoint after adopting a response with a %s", async (change) => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    await emit(checkpoint("attachment-0", "old screen", "5"));
    await emit({ event_type: "attachment_exited", attachment_id: "attachment-0", reason: "connection_closed", exit_code: null, next_sequence: "5", received_sequence: "5" });
    const authoritative = change === "moved pane" ? { ...first, session_id: "promoted", view_id: "promoted-view" } : first;
    const open = api.openAttachment.getMockImplementation()!;
    api.openAttachment.mockImplementation(async (...args) => {
      const response = await open(...args);
      response.attached.session = authoritative;
      const attachment_id = response.attached.attachment_id;
      if (args[0].resume_from !== null) {
        if (change === "lost cache") renderer.retainSessions(new Set());
        args[1]({ event_type: "output", attachment_id, event_id: "rejected-delta", sequence_start: "5", sequence_end: "8", data_base64: btoa("wrong delta") });
      } else args[1](checkpoint(attachment_id, "authoritative screen", "9"));
      return response;
    });

    await act(async () => { await result.current.reconnect(); });
    await waitFor(() => expect(result.current.state.applied_sequence).toBe("9"));
    expect(api.openAttachment.mock.calls.slice(1).map(([request]) => ({ session: request.session, resume_from: request.resume_from }))).toEqual([
      { session: first.terminal_id, resume_from: "5" },
      { session: first.terminal_id, resume_from: null },
    ]);
    expect(api.detachAttachment).toHaveBeenCalledExactlyOnceWith({ attachment_id: "attachment-1" });
    expect(result.current.state.session?.session_id).toBe(authoritative.session_id);
    expect(line(visibleTerminal().terminal)).toBe("authoritative screen");
    await act(async () => { channels.get("attachment-1")!(checkpoint("attachment-1", "late rejected screen", "10")); });
    expect(line(visibleTerminal().terminal)).toBe("authoritative screen");
    expect(api.acknowledgeAttachmentEvent.mock.calls.map(([request]) => request.attachment_id)).not.toContain("attachment-1");
  });

  it("cancels the checkpoint fallback while detaching its rejected stream", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    await emit(checkpoint("attachment-0", "old screen", "5"));
    await emit({ event_type: "attachment_exited", attachment_id: "attachment-0", reason: "connection_closed", exit_code: null, next_sequence: "5", received_sequence: "5" });
    const open = api.openAttachment.getMockImplementation()!;
    api.openAttachment.mockImplementationOnce(async (...args) => {
      const response = await open(...args);
      renderer.retainSessions(new Set());
      return response;
    });
    let finishDetach!: () => void;
    api.detachAttachment.mockImplementationOnce(() => new Promise<void>((resolve) => { finishDetach = resolve; }));
    let reconnecting!: Promise<void>;
    await act(async () => { reconnecting = result.current.reconnect(); });
    await waitFor(() => expect(api.detachAttachment).toHaveBeenCalledWith({ attachment_id: "attachment-1" }));
    await act(async () => { await result.current.detach(); finishDetach(); await reconnecting; });
    expect(api.openAttachment).toHaveBeenCalledTimes(2);
    expect(result.current.state.phase).toBe("idle");
  });

  it("resolves root opens afresh instead of reusing a stale terminal ID or sequence", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "old-terminal", "40"));
    await act(async () => { await result.current.connect(first); });
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: "first", resume_from: null });
  });

  it("invalidates the cached output cursor when selecting another terminal in the same root", async () => {
    renderer.activateSession(first);
    await renderer.write(new TextEncoder().encode("first terminal"), "40");
    const previous = visibleTerminal();
    renderer.activateSession({ ...first, terminal_id: "replacement-terminal" });
    expect(renderer.resumeSequence()).toBeNull();
    expect(visibleTerminal()).not.toBe(previous);
    await waitFor(() => expect(previous.dispose).toHaveBeenCalledOnce());
  });

  it("moves an alias cache to the recovered host ID with its buffer and resume cursor", async () => {
    const previous: SessionSummary = {
      ...first,
      target: { kind: "ssh", host_id: "alias", destination: "new-ip" },
    };
    const recovered: SessionSummary = {
      ...previous,
      target: { kind: "ssh", host_id: "canonical", destination: "new-ip" },
    };
    renderer.activateSession(previous);
    await renderer.write(new TextEncoder().encode("cached remote output"), "20");
    const cached = visibleTerminal();
    renderer.remapSessions(new Map([[sessionKey(previous), sessionKey(recovered)]]));
    renderer.retainSessions(new Set([sessionKey(recovered)]));
    renderer.activateSession(recovered);
    expect(visibleTerminal()).toBe(cached);
    expect(line(cached.terminal)).toBe("cached remote output");
    expect(renderer.resumeSequence()).toBe("20");
    expect(cached.dispose).not.toHaveBeenCalled();
  });

  it("reactivates the same buffer and resumes only missing output, including sequence zero", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "first"));
    const saved = visibleTerminal();

    await act(async () => { await result.current.connect(second, { terminal_id: second.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "second"));
    const instance_count = xterm.instances.length;
    const open = api.openAttachment.getMockImplementation()!;
    let finish_open!: () => void;
    const pending_open = new Promise<void>((resolve) => { finish_open = resolve; });
    api.openAttachment.mockImplementationOnce(async (...args) => {
      await pending_open;
      return open(...args);
    });
    let returning!: Promise<void>;
    await act(async () => { returning = result.current.connect(first, { terminal_id: first.terminal_id }); });

    // The cached view is already visible while the transport is still opening.
    expect(visibleTerminal()).toBe(saved);
    expect(line(saved.terminal)).toBe("first");
    expect(result.current.state.phase).toBe("connecting");
    expect(result.current.state.applied_sequence).toBe("0");
    await act(async () => {
      finish_open();
      await returning;
    });
    expect(saved.dispose).not.toHaveBeenCalled();
    expect(xterm.instances).toHaveLength(instance_count);
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: "first-terminal", resume_from: "0" });
    expect(result.current.state.applied_sequence).toBe("0");
    await emit({
      event_type: "output",
      attachment_id: result.current.state.attachment_id!,
      event_id: "missed-output",
      sequence_start: "0",
      sequence_end: "5",
      data_base64: btoa(" tail"),
    });
    expect(line(saved.terminal)).toBe("first tail");
  });

  it("preserves an incomplete UTF-8 character across deactivation", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "amount: "));
    const saved = visibleTerminal();
    await emit({
      event_type: "output",
      attachment_id: result.current.state.attachment_id!,
      event_id: "partial-utf8",
      sequence_start: "0",
      sequence_end: "2",
      data_base64: btoa(String.fromCharCode(0xe2, 0x82)),
    });
    await act(async () => { await result.current.connect(second, { terminal_id: second.terminal_id }); });
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    expect(api.openAttachment.mock.lastCall?.[0].resume_from).toBe("2");
    await emit({
      event_type: "output",
      attachment_id: result.current.state.attachment_id!,
      event_id: "remaining-utf8",
      sequence_start: "2",
      sequence_end: "3",
      data_base64: btoa(String.fromCharCode(0xac)),
    });
    expect(line(saved.terminal)).toBe("amount: €");
  });

  it("finishes an in-flight write before choosing the cached resume position", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "first"));
    const saved = visibleTerminal();
    const write = saved.terminal.write.bind(saved.terminal);
    let finish: (() => void) | undefined;
    vi.spyOn(saved.terminal, "write").mockImplementation((data, callback) => {
      finish = () => write(data, callback);
    });
    const old_attachment = result.current.state.attachment_id!;
    await act(async () => {
      channels.get(old_attachment)!({
        event_type: "output",
        attachment_id: old_attachment,
        event_id: "in-flight",
        sequence_start: "0",
        sequence_end: "1",
        data_base64: btoa("!"),
      });
    });
    expect(finish).toBeDefined();
    let switching: Promise<void>;
    let returning: Promise<void>;
    await act(async () => {
      switching = result.current.connect(second, { terminal_id: second.terminal_id });
      returning = result.current.connect(first, { terminal_id: first.terminal_id });
    });
    expect(visibleTerminal()).toBe(saved);
    await act(async () => {
      finish!();
      await Promise.all([switching!, returning!]);
    });
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: "first-terminal", resume_from: "1" });
    expect(line(saved.terminal)).toBe("first!");
    expect(api.acknowledgeAttachmentEvent).not.toHaveBeenCalledWith({
      attachment_id: old_attachment,
      event_id: "in-flight",
    });
  });

  it("replaces a cached buffer when the server requires a checkpoint", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    await emit(checkpoint(result.current.state.attachment_id!, "old"));
    const saved = visibleTerminal();
    await act(async () => { await result.current.connect(second, { terminal_id: second.terminal_id }); });
    await act(async () => { await result.current.connect(first, { terminal_id: first.terminal_id }); });
    const replacement = checkpoint(result.current.state.attachment_id!, "fresh", "20");
    replacement.history_gap = true;
    await emit(replacement);
    expect(line(visibleTerminal().terminal)).toBe("fresh");
    expect(saved.dispose).toHaveBeenCalledOnce();
    expect(result.current.state.history_gap).toBe(true);
    expect(renderer.resumeSequence()).toBe("20");
  });

  it("keeps host identities separate and releases closed tabs and local restart state", async () => {
    const remote: SessionSummary = { ...first, target: { kind: "ssh", destination: "host" } };
    renderer.activateSession(first);
    await renderer.write(new TextEncoder().encode("local"), "5");
    const local = visibleTerminal();
    renderer.activateSession(remote);
    expect(renderer.resumeSequence()).toBeNull();
    await renderer.write(new TextEncoder().encode("remote"), "6");
    const ssh = visibleTerminal();
    renderer.forgetLocalSessions();
    await Promise.resolve();
    expect(local.dispose).toHaveBeenCalledOnce();
    expect(renderer.resumeSequence()).toBe("6");
    expect(ssh.dispose).not.toHaveBeenCalled();
    renderer.activateSession(first);
    expect(renderer.resumeSequence()).toBeNull();
    renderer.retainSessions(new Set([sessionKey(first)]));
    await Promise.resolve();
    expect(ssh.dispose).toHaveBeenCalledOnce();
    renderer.activateSession(remote);
    expect(renderer.resumeSequence()).toBeNull();
    expect(line(visibleTerminal().terminal)).toBe("");
  });

  it("does not resurrect an invalidated cursor when a pending write finishes", async () => {
    renderer.activateSession(first);
    const pending = renderer.write(new TextEncoder().encode("pending"), "7");
    renderer.invalidateResumeSequence();
    await pending;
    renderer.activateSession(second);
    renderer.activateSession(first);
    expect(line(visibleTerminal().terminal)).toBe("pending");
    expect(renderer.resumeSequence()).toBeNull();
  });
});


describe("durable terminal previews", () => {
  it("retains a root preview while adopting its resolved terminal before the live checkpoint", async () => {
    api.sessionCache.mockResolvedValue({ kind: "loaded", cache: {
      terminal_id: first.terminal_id, checkpoint: checkpoint("saved", "saved preview", "7").checkpoint,
      history: [], history_gap: false,
    } });
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    expect(line(visibleTerminal().terminal)).toBe("saved preview");
    expect(api.openAttachment.mock.lastCall?.[0].resume_from).toBeNull();
    await emit(checkpoint(result.current.state.attachment_id!, "authoritative screen", "9"));
    expect(line(visibleTerminal().terminal)).toBe("authoritative screen");
  });

  it("shows the saved screen when the daemon is offline without using it as an unsafe replay cursor", async () => {
    api.sessionCache.mockResolvedValue({ kind: "loaded", cache: {
      terminal_id: first.terminal_id, checkpoint: checkpoint("saved", "saved current", "7").checkpoint,
      history: [], history_gap: false,
    } });
    api.openAttachment.mockRejectedValue({ code: "session_not_found", message: "Session no longer exists" });
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    expect(line(visibleTerminal().terminal)).toBe("saved current");
    expect(result.current.state.applied_sequence).toBe("7");
    expect(result.current.state.phase).toBe("error");
    expect(api.openAttachment).toHaveBeenCalledWith(expect.objectContaining({ resume_from: null }), expect.any(Function), expect.any(AbortSignal));
  });

  it("reports a disk-cache failure before opening a live channel", async () => {
    api.sessionCache.mockRejectedValue(new Error("Local cache is unreadable"));
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    expect(result.current.state.error_code).toBe("local_cache_failed");
    expect(result.current.state.message).toBe("Local cache is unreadable");
    expect(api.openAttachment).not.toHaveBeenCalled();
  });
});

describe("opened session channels", () => {
  it("keeps the selected scoped tab visible when its cache disappears during reconnect", async () => {
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<Harness />);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await emit(checkpoint("attachment-0", "old screen", "5"));
    const previous = visibleTerminal();
    const open = api.openAttachment.getMockImplementation()!;
    api.openAttachment.mockImplementationOnce(async (...args) => {
      const response = await open(...args);
      renderer.retainSessions(new Set());
      return response;
    });
    await emit({ event_type: "attachment_exited", attachment_id: "attachment-0", reason: "connection_closed", exit_code: null, next_sequence: "5", received_sequence: "5" });
    await waitFor(() => expect(api.openAttachment).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    expect(api.openAttachment.mock.lastCall?.[0].resume_from).toBeNull();
    await emit(checkpoint("attachment-2", "restored selected screen", "9"));
    expect(line(visibleTerminal().terminal)).toBe("restored selected screen");
    expect(previous.dispose).toHaveBeenCalledOnce();
    expect(renderer.resumeSequence()).toBe("9");
  });

  it("keeps a moved background pane's live cache in its opened tab across tab switches and retention", async () => {
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<Harness />);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await emit(checkpoint("attachment-0", "first screen", "5"));
    const original = visibleTerminal();
    act(() => { void attachments.connect(second); });
    await waitFor(() => expect(attachments.state.session?.session_id).toBe(second.session_id));
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await emit(checkpoint("attachment-1", "second screen", "8"));
    const foreground = visibleTerminal();
    const open = api.openAttachment.getMockImplementation()!;
    api.openAttachment.mockImplementation(async (...args) => {
      const response = await open(...args);
      if (args[0].session === first.terminal_id) response.attached.session = { ...first, session_id: "promoted", view_id: "promoted-view" };
      return response;
    });

    await emit({ event_type: "attachment_exited", attachment_id: "attachment-0", reason: "connection_closed", exit_code: null, next_sequence: "5", received_sequence: "5" });
    await waitFor(() => expect(attachments.states.find((state) => state.session?.session_id === "promoted")?.phase).toBe("attached"));
    expect(api.openAttachment.mock.lastCall?.[0].resume_from).toBe("5");
    expect(visibleTerminal() === foreground).toBe(true);
    await emit({ event_type: "output", attachment_id: "attachment-2", event_id: "moved-background", sequence_start: "5", sequence_end: "10", data_base64: btoa(" moved") });
    const open_keys = new Set([sessionKey(first), sessionKey(second)]);
    act(() => { attachments.retainSessions(open_keys); renderer.retainSessions(open_keys); });
    await act(async () => { await attachments.connect(first); });
    expect(visibleTerminal() === original).toBe(true);
    expect(line(original.terminal)).toBe("first screen moved");
    await act(async () => { await attachments.connect(second); await attachments.connect(first); });
    await emit({ event_type: "output", attachment_id: "attachment-2", event_id: "moved-visible", sequence_start: "10", sequence_end: "15", data_base64: btoa(" live") });
    expect(line(visibleTerminal().terminal)).toBe("first screen moved live");
    expect(renderer.resumeSequence()).toBe("15");
    expect(original.dispose).not.toHaveBeenCalled();
    expect(line(foreground.terminal)).toBe("second screen");
    expect(api.openAttachment).toHaveBeenCalledTimes(3);
  });

  it("reconnects a background root stream without clearing its screen or changing the visible tab", async () => {
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<Harness />);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await emit(checkpoint("attachment-0", "first screen", "5"));
    const background = visibleTerminal();
    act(() => { void attachments.connect(second); });
    await waitFor(() => expect(attachments.state.session?.session_id).toBe(second.session_id));
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await emit(checkpoint("attachment-1", "visible second", "8"));
    const foreground = visibleTerminal();

    await emit({ event_type: "attachment_exited", attachment_id: "attachment-0", reason: "connection_closed", exit_code: null, next_sequence: "5", received_sequence: "5" });
    await waitFor(() => expect(api.openAttachment).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(attachments.states.find((state) => state.session?.session_id === first.session_id)?.phase).toBe("attached"));
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: first.terminal_id, resume_from: "5" });
    expect(visibleTerminal()).toBe(foreground);
    await emit({ event_type: "output", attachment_id: "attachment-2", event_id: "background-resumed", sequence_start: "5", sequence_end: "10", data_base64: btoa(" new output") });
    expect(line(foreground.terminal)).toBe("visible second");
    await act(async () => { await attachments.connect(first); });
    expect(visibleTerminal() === background).toBe(true);
    expect(line(background.terminal)).toBe("first screen new output");
    expect(background.dispose).not.toHaveBeenCalled();
  });

  it("keeps receiving background output and switches without reopening either channel", async () => {
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<Harness />);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await emit(checkpoint("attachment-0", "first"));
    act(() => { void attachments.connect(second); });
    await waitFor(() => expect(attachments.state.session?.session_id).toBe("second"));
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await emit(checkpoint("attachment-1", "second"));
    await emit({ event_type: "output", attachment_id: "attachment-0", event_id: "background-output", sequence_start: "0", sequence_end: "8", data_base64: btoa(" background") });
    expect(line(visibleTerminal().terminal)).toBe("second");
    await act(async () => { await attachments.connect(first); });
    expect(line(visibleTerminal().terminal)).toBe("first background");
    expect(renderer.resumeSequence()).toBe("8");
    expect(api.openAttachment).toHaveBeenCalledTimes(2);
    expect(api.detachAttachment).not.toHaveBeenCalled();
    await act(async () => { attachments.handleInput(new TextEncoder().encode("pwd\r")); });
    await waitFor(() => expect(api.sendInput).toHaveBeenCalledWith({ attachment_id: "attachment-0", data_base64: btoa("pwd\r") }));
    await act(async () => { await attachments.detach(); });
    await emit(checkpoint("attachment-1", "background second", "9"));
    await act(async () => { await attachments.connect(second); });
    expect(line(visibleTerminal().terminal)).toBe("background second");
    expect(api.openAttachment).toHaveBeenCalledTimes(2);
    await act(async () => { await attachments.closeSession(first); });
    await waitFor(() => expect(api.detachAttachment).toHaveBeenCalledWith({ attachment_id: "attachment-0" }));
    expect(api.detachAttachment).not.toHaveBeenCalledWith({ attachment_id: "attachment-1" });
    await emit(checkpoint("attachment-1", "still connected", "10"));
    expect(line(visibleTerminal().terminal)).toBe("still connected");
  });

  it("preserves a recovered session's cache when returning to its tab", async () => {
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<Harness />);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await act(async () => { await attachments.reconnect(); });
    await emit(checkpoint("attachment-1", "recovered", "7"));
    act(() => { void attachments.connect(second); });
    await waitFor(() => expect(attachments.state.session?.session_id).toBe("second"));
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await act(async () => { await attachments.connect(first); });
    expect(line(visibleTerminal().terminal)).toBe("recovered");
    expect(renderer.resumeSequence()).toBe("7");
    expect(api.openAttachment).toHaveBeenCalledTimes(3);
  });

  it("archives a removed interactive session after detaching its channel", async () => {
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<Harness />);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    api.sessionCache.mockImplementation(async (action) => {
      if (action.kind === "archive") expect(api.detachAttachment).toHaveBeenCalledWith({ attachment_id: "attachment-0" });
      return { kind: "archived" };
    });
    act(() => attachments.retainSessions(new Set()));
    await waitFor(() => expect(api.sessionCache).toHaveBeenCalledWith({ kind: "archive", host_key: "local", session_id: "first", reason: "Tab closed" }));
    expect(attachments.session_keys.size).toBe(0);
  });

  it("reports an archive failure after the final attachment is removed", async () => {
    const store = new NotificationStore();
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      useWorkbenchNotifications(store, {
        workspace_error: null, workspace_ready: true, keybindings_error: null, session_error: null,
        targets: [], target_errors: new Map(), storage_error: attachments.storage_error,
        task_error: null, definitions_error: null, task_status: null, tasks: [], tasks_loaded: false,
      });
      return <>{attachments.controllers}</>;
    }
    render(<NotificationProvider store={store}><Harness /></NotificationProvider>);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    api.sessionCache.mockImplementation(async (action) => {
      if (action.kind === "archive") throw new Error("Archive failed: disk full");
      return { kind: "recorded" };
    });
    act(() => attachments.retainSessions(new Set()));
    await waitFor(() => expect(attachments.storage_error).toBe("Archive failed: disk full"));
    expect(attachments.state.session).toBeNull();
    expect(attachments.state.message).toBeNull();
    expect(store.snapshot().entries).toContainEqual(expect.objectContaining({ title: "Session history", severity: "error", message: "Archive failed: disk full" }));
  });

  it("reports failures from a background session's real attachment controller", async () => {
    const store = new NotificationStore();
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<NotificationProvider store={store}><Harness /></NotificationProvider>);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    const background_id = attachments.state.attachment_id!;
    act(() => { void attachments.connect(second); });
    await waitFor(() => expect(attachments.state.session?.session_id).toBe("second"));
    await act(async () => channels.get(background_id)!({
      event_type: "attachment_error", attachment_id: background_id,
      code: "ssh_authentication_required", message: "Background authentication expired",
    }));
    expect(store.snapshot().entries).toContainEqual(expect.objectContaining({ severity: "error", message: "Background authentication expired" }));
    expect(attachments.state.session?.session_id).toBe("second");
  });

  it("reports identical failures from explicit retries outside the notification card", async () => {
    const store = new NotificationStore();
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<NotificationProvider store={store}><Harness /></NotificationProvider>);
    act(() => { void attachments.connect(first); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    const attachment_id = attachments.state.attachment_id!;
    const failure = { code: "ssh_authentication_required", message: "Authentication expired" };
    await act(async () => channels.get(attachment_id)!({ event_type: "attachment_error", attachment_id, ...failure }));
    act(() => store.dismiss(store.snapshot().entries[0].id));
    api.openAttachment.mockRejectedValueOnce(failure);
    await act(async () => { await attachments.reconnect(); });
    await waitFor(() => expect(store.snapshot().entries).toContainEqual(expect.objectContaining({ message: failure.message, toast_visible: true })));
  });

  it("disconnects background streams only for the requested host", async () => {
    const remote: SessionSummary = { ...first, target: { kind: "ssh", host_id: "remote-host", destination: "example.test" } };
    const open = api.openAttachment.getMockImplementation()!;
    api.openAttachment.mockImplementation(async (...args) => {
      const result = await open(...args);
      if (args[0].target.kind === "ssh") result.attached.session = remote;
      return result;
    });
    let attachments!: ReturnType<typeof useSessionAttachments>;
    function Harness() {
      attachments = useSessionAttachments(renderer);
      return <>{attachments.controllers}</>;
    }
    render(<Harness />);
    act(() => { void attachments.connect(remote); });
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    act(() => { void attachments.connect(second); });
    await waitFor(() => expect(attachments.state.session?.session_id).toBe("second"));
    await waitFor(() => expect(attachments.state.phase).toBe("attached"));
    await act(async () => { await attachments.disconnectHost("remote-host"); });
    expect(api.detachAttachment).toHaveBeenCalledExactlyOnceWith({ attachment_id: "attachment-0" });
    expect(attachments.state.session?.session_id).toBe("second");
    expect(attachments.state.phase).toBe("attached");
    await emit(checkpoint("attachment-1", "local unaffected"));
    expect(line(visibleTerminal().terminal)).toBe("local unaffected");
  });

});
