// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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
}));
vi.mock("../../lib/tauri", () => api);

const size = { columns: 80, rows: 24, pixel_width: null, pixel_height: null };
const first: SessionSummary = {
  target: { kind: "local" },
  session_id: "first",
  name: "first",
  status: "running",
  next_sequence: "0",
  terminal_size: size,
};
const second: SessionSummary = { ...first, session_id: "second", name: "second" };
const sessions = [first, second];
let renderer: XtermRenderer;
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
        session: sessions.find((session) => session.session_id === request.session),
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
  renderer.dispose();
  container.remove();
  await Promise.resolve();
});

describe("pending remote attachments", () => {
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
    expect(result.current.state.session).toEqual(second);
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
    await act(async () => { await result.current.connect(first); });
    await emit(checkpoint(result.current.state.attachment_id!, "first"));
    const saved = visibleTerminal();

    await act(async () => { await result.current.connect(second); });
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
    await act(async () => { returning = result.current.connect(first); });

    // The cached view is already visible while the transport is still opening.
    expect(visibleTerminal()).toBe(saved);
    expect(line(saved.terminal)).toBe("first");
    expect(result.current.state.phase).toBe("reconnecting");
    expect(result.current.state.applied_sequence).toBe("0");
    await act(async () => {
      finish_open();
      await returning;
    });
    expect(saved.dispose).not.toHaveBeenCalled();
    expect(xterm.instances).toHaveLength(instance_count);
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: "first", resume_from: "0" });
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
    await act(async () => { await result.current.connect(first); });
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
    await act(async () => { await result.current.connect(second); });
    await act(async () => { await result.current.connect(first); });
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
    await act(async () => { await result.current.connect(first); });
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
      switching = result.current.connect(second);
      returning = result.current.connect(first);
    });
    expect(visibleTerminal()).toBe(saved);
    await act(async () => {
      finish!();
      await Promise.all([switching!, returning!]);
    });
    expect(api.openAttachment.mock.lastCall?.[0]).toMatchObject({ session: "first", resume_from: "1" });
    expect(line(saved.terminal)).toBe("first!");
    expect(api.acknowledgeAttachmentEvent).not.toHaveBeenCalledWith({
      attachment_id: old_attachment,
      event_id: "in-flight",
    });
  });

  it("replaces a cached buffer when the server requires a checkpoint", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => { await result.current.connect(first); });
    await emit(checkpoint(result.current.state.attachment_id!, "old"));
    const saved = visibleTerminal();
    await act(async () => { await result.current.connect(second); });
    await act(async () => { await result.current.connect(first); });
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
