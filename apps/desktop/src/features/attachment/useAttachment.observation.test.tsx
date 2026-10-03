// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttachmentEvent, OpenAttachmentRequest, SessionSummary } from "../../lib/types";
import type { AttachmentRenderer } from "../terminal/XtermRenderer";
import { useAttachment } from "./useAttachment";

const api = vi.hoisted(() => ({
  openAttachment: vi.fn(), acknowledgeAttachmentEvent: vi.fn(), detachAttachment: vi.fn(), requestAttachmentCheckpoint: vi.fn(),
  acquireAttachmentLease: vi.fn(), releaseAttachmentLease: vi.fn(), resizeAttachment: vi.fn(),
  sendInput: vi.fn(), sessionCache: vi.fn(),
}));
vi.mock("../../lib/tauri", () => api);

const size = { columns: 90, rows: 30, pixel_width: null, pixel_height: null };
const session: SessionSummary = {
  target: { kind: "local" }, session_id: "fixture", terminal_id: "terminal", name: "fixture",
  status: "running", terminal_size: size, next_sequence: "0", last_seen_at_ms: 100,
};
let renderer: AttachmentRenderer;
let channels: ((event: AttachmentEvent) => void)[];

beforeEach(() => {
  vi.resetAllMocks();
  channels = [];
  renderer = {
    activateSession: vi.fn(), adoptSession: vi.fn(), resumeSequence: () => null,
    invalidateResumeSequence: vi.fn(), write: vi.fn().mockResolvedValue(undefined),
    restoreCheckpoint: vi.fn().mockResolvedValue(undefined), recreate: vi.fn().mockResolvedValue(undefined),
    resize: vi.fn().mockResolvedValue(undefined), proposeDimensions: () => ({ columns: 90, rows: 30 }),
    observeDimensions: () => () => undefined, focus: vi.fn(),
  };
  api.sessionCache.mockResolvedValue({ kind: "loaded", cache: null });
  api.detachAttachment.mockResolvedValue(undefined);
  api.requestAttachmentCheckpoint.mockResolvedValue(undefined);
  api.acknowledgeAttachmentEvent.mockResolvedValue(undefined);
  api.openAttachment.mockImplementation(async (request: OpenAttachmentRequest, on_event: (event: AttachmentEvent) => void) => {
    const attachment_id = `attachment-${channels.length}`;
    channels.push(on_event);
    return { channel: {}, attached: {
      attachment_id, session: { ...session, target: request.target }, replay_from: "0", history_gap: false,
      terminal_size_mismatch: false, input_lease: { held: true, owned_by_client: true },
      layout_lease: { held: false, owned_by_client: false }, shell_state: {
        shell_type: "unknown", cwd: null, running_command: null, prompt_phase: "unknown",
        tui_hint: "unknown", revision: "0", observed_sequence: "0",
      },
    } };
  });
});
afterEach(cleanup);

async function emit(event: AttachmentEvent, channel = channels[channels.length - 1]!) {
  await act(async () => { channel(event); });
}

describe("attachment observation evidence", () => {
  it("advances only from valid matching native observations", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => result.current.connect(session));
    expect(result.current.state.session).toMatchObject({ last_seen_at_ms: 100, terminal_size_known: true });
    await emit({ event_type: "session_observed", attachment_id: "wrong", last_seen_at_ms: 900 });
    expect(result.current.state.session?.last_seen_at_ms).toBe(100);
    await emit({ event_type: "session_observed", attachment_id: "attachment-0", last_seen_at_ms: 200 });
    const observed = result.current.state.session;
    for (const last_seen_at_ms of [200, 150, 0, -1, NaN, 8_640_000_000_000_001]) {
      await emit({ event_type: "session_observed", attachment_id: "attachment-0", last_seen_at_ms });
      expect(result.current.state.session).toBe(observed);
    }
  });

  it("ignores obsolete-generation observations even when they name the new attachment", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => result.current.connect(session));
    const obsolete = channels[0];
    await act(async () => result.current.reconnect());
    await emit({ event_type: "session_observed", attachment_id: "attachment-1", last_seen_at_ms: 900 }, obsolete);
    expect(result.current.state.session?.last_seen_at_ms).toBe(100);
    await emit({ event_type: "session_observed", attachment_id: "attachment-1", last_seen_at_ms: 200 });
    expect(result.current.state.session?.last_seen_at_ms).toBe(200);
  });

  it("retains the last observation across failure and unsuccessful retry", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => result.current.connect(session));
    await emit({ event_type: "session_observed", attachment_id: "attachment-0", last_seen_at_ms: 200 });
    await emit({ event_type: "attachment_error", attachment_id: "attachment-0", code: "ssh_authentication_required", message: "fixture unavailable" });
    api.openAttachment.mockRejectedValueOnce({ code: "ssh_authentication_required", message: "fixture unavailable" });
    await act(async () => result.current.reconnect());
    expect(result.current.state).toMatchObject({ phase: "error", session: { last_seen_at_ms: 200, terminal_size: size } });
  });

  it("does not turn a disk preview or failed connect into a new observation", async () => {
    const cached_size = { ...size, columns: 150 };
    api.sessionCache.mockResolvedValue({ kind: "loaded", cache: {
      checkpoint: { terminal_size: cached_size, sequence: "0", payload_base64: "", input_prefix_base64: "" },
      history: [], history_gap: false,
    } });
    api.openAttachment.mockRejectedValue({ code: "ssh_authentication_required", message: "fixture unavailable" });
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => result.current.connect(session));
    expect(renderer.restoreCheckpoint).toHaveBeenCalled();
    expect(result.current.state.session).toMatchObject({ terminal_size: size, last_seen_at_ms: 100 });
  });

  it("carries authoritative geometry through the next coalesced observation", async () => {
    const { result } = renderHook(() => useAttachment(renderer));
    await act(async () => result.current.connect(session));
    const terminal_size = { ...size, columns: 130, rows: 44 };
    await emit({ event_type: "pty_geometry_changed", attachment_id: "attachment-0", event_id: "geometry", observed_sequence: "0", terminal_size });
    await emit({ event_type: "session_observed", attachment_id: "attachment-0", last_seen_at_ms: 300 });
    expect(result.current.state.session).toMatchObject({ terminal_size, terminal_size_known: true, last_seen_at_ms: 300 });
  });
});
