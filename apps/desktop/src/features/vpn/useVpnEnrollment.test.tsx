// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode, type ReactNode } from "react";
import { beginVpnEnrollment, cancelVpnEnrollment, openVpnSignIn, vpnEnrollmentStatus } from "../../lib/tauri";
import type { VpnEnrollmentSnapshot } from "../../lib/types";
import { useVpnEnrollment, VPN_ENROLLMENT_INTERVAL_MS } from "./useVpnEnrollment";

vi.mock("../../lib/tauri", () => ({
  beginVpnEnrollment: vi.fn(), cancelVpnEnrollment: vi.fn(), openVpnSignIn: vi.fn(), vpnEnrollmentStatus: vi.fn(),
}));

const input = { name: "Tailnet", hostname: null, accept_routes: false };
const pending: VpnEnrollmentSnapshot = {
  enrollment_id: "draft-one", connection_id: "connection-one", error: null,
  status: { provider: "tailscale", vpn_id: "connection-one", connection_id: "connection-one", state: "starting", running: false,
    endpoint: null, container_name: null, auth_url: "https://login.tailscale.com/a/example" },
};
const connected: VpnEnrollmentSnapshot = { ...pending, status: { ...pending.status, state: "connected", running: true, auth_url: null,
  endpoint: "socks5h://127.0.0.1:49152", username: "sample@example.test", tailnet: "example.test" } };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

function setup() {
  const on_close = vi.fn();
  const on_save = vi.fn().mockResolvedValue(true);
  const on_connection_id = vi.fn();
  return { ...renderHook(() => useVpnEnrollment({ on_close, on_save, on_connection_id }), {
    wrapper: ({ children }: { children: ReactNode }) => <StrictMode>{children}</StrictMode>,
  }), on_close, on_save, on_connection_id };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(beginVpnEnrollment).mockResolvedValue(pending);
  vi.mocked(vpnEnrollmentStatus).mockResolvedValue(pending);
  vi.mocked(openVpnSignIn).mockResolvedValue(undefined);
  vi.mocked(cancelVpnEnrollment).mockResolvedValue(undefined);
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("Tailscale enrollment", () => {
  it("opens sign-in once, observes authentication, and saves only after it is ready", async () => {
    vi.useFakeTimers();
    const { result, on_save, on_connection_id, unmount } = setup();
    await act(async () => { await result.current.begin(input); });
    expect(beginVpnEnrollment).toHaveBeenCalledExactlyOnceWith(input);
    expect(on_connection_id).toHaveBeenCalledWith("connection-one");
    expect(openVpnSignIn).toHaveBeenCalledExactlyOnceWith("connection-one");
    await act(async () => { expect(await result.current.save()).toBe(false); });
    expect(on_save).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(VPN_ENROLLMENT_INTERVAL_MS * 2); });
    expect(openVpnSignIn).toHaveBeenCalledOnce();
    vi.mocked(vpnEnrollmentStatus).mockResolvedValue(connected);
    await act(async () => { await vi.advanceTimersByTimeAsync(VPN_ENROLLMENT_INTERVAL_MS); });
    expect(result.current.ready).toBe(true);
    await act(async () => { expect(await result.current.save()).toBe(true); });
    expect(on_save).toHaveBeenCalledExactlyOnceWith("draft-one");
    unmount();
    await act(async () => {});
    expect(cancelVpnEnrollment).not.toHaveBeenCalled();
  });

  it("cancels a late begin response without opening its browser", async () => {
    const begin = deferred<VpnEnrollmentSnapshot>();
    vi.mocked(beginVpnEnrollment).mockReturnValue(begin.promise);
    const { result, on_close } = setup();
    let starting!: Promise<void>;
    let cancelling!: Promise<void>;
    act(() => { starting = result.current.begin(input); });
    await waitFor(() => expect(beginVpnEnrollment).toHaveBeenCalledOnce());
    act(() => { cancelling = result.current.cancel(); });
    expect(result.current.cancelling).toBe(true);
    expect(on_close).not.toHaveBeenCalled();
    await act(async () => { begin.resolve(pending); await starting; await cancelling; });
    expect(cancelVpnEnrollment).toHaveBeenCalledExactlyOnceWith("draft-one");
    expect(on_close).toHaveBeenCalledOnce();
    expect(openVpnSignIn).not.toHaveBeenCalled();
  });

  it("discards an unfinished enrollment after its editor unmounts", async () => {
    const begin = deferred<VpnEnrollmentSnapshot>();
    vi.mocked(beginVpnEnrollment).mockReturnValue(begin.promise);
    const { result, unmount } = setup();
    let starting!: Promise<void>;
    act(() => { starting = result.current.begin(input); });
    await waitFor(() => expect(beginVpnEnrollment).toHaveBeenCalledOnce());
    unmount();
    await act(async () => { begin.resolve(pending); await starting; });
    await waitFor(() => expect(cancelVpnEnrollment).toHaveBeenCalledExactlyOnceWith("draft-one"));
    expect(openVpnSignIn).not.toHaveBeenCalled();
  });

  it("keeps browser errors visible and permits an explicit browser retry", async () => {
    vi.mocked(openVpnSignIn).mockRejectedValueOnce(new Error("Browser unavailable")).mockResolvedValueOnce(undefined);
    const { result } = setup();
    await act(async () => { await result.current.begin(input); });
    await waitFor(() => expect(result.current.browser_error).toBe("Browser unavailable"));
    await act(async () => { await result.current.refresh(); });
    expect(openVpnSignIn).toHaveBeenCalledOnce();
    await act(async () => { await result.current.openBrowser(); });
    expect(openVpnSignIn).toHaveBeenCalledTimes(2);
    expect(result.current.browser_error).toBeNull();
  });

  it("retains authenticated enrollment after save failure and defers cleanup through a successful save", async () => {
    vi.mocked(beginVpnEnrollment).mockResolvedValue(connected);
    const { result, on_save, unmount } = setup();
    await act(async () => { await result.current.begin(input); });
    on_save.mockResolvedValueOnce(false);
    await act(async () => { expect(await result.current.save()).toBe(false); });
    expect(result.current.ready).toBe(true);
    expect(cancelVpnEnrollment).not.toHaveBeenCalled();
    const saving = deferred<boolean>();
    on_save.mockReturnValueOnce(saving.promise);
    let finished!: Promise<boolean>;
    act(() => { finished = result.current.save(); });
    unmount();
    await act(async () => { saving.resolve(true); await finished; });
    expect(cancelVpnEnrollment).not.toHaveBeenCalled();
  });

  it("cleans up if the editor disappears during a failed save", async () => {
    vi.mocked(beginVpnEnrollment).mockResolvedValue(connected);
    const { result, on_save, unmount } = setup();
    await act(async () => { await result.current.begin(input); });
    const saving = deferred<boolean>();
    on_save.mockReturnValueOnce(saving.promise);
    let finished!: Promise<boolean>;
    act(() => { finished = result.current.save(); });
    unmount();
    await act(async () => { saving.resolve(false); await finished; });
    expect(cancelVpnEnrollment).toHaveBeenCalledExactlyOnceWith("draft-one");
  });

  it("retains startup errors and discards the old draft before retrying", async () => {
    vi.mocked(beginVpnEnrollment).mockResolvedValueOnce({ ...pending, error: { code: "vpn_failed", message: "Container unavailable" }, status: { ...pending.status, auth_url: null } });
    const { result } = setup();
    await act(async () => { await result.current.begin(input); });
    expect(result.current.snapshot?.error?.message).toBe("Container unavailable");
    expect(openVpnSignIn).not.toHaveBeenCalled();
    await act(async () => { await result.current.begin(input); });
    expect(cancelVpnEnrollment).toHaveBeenCalledExactlyOnceWith("draft-one");
    expect(beginVpnEnrollment).toHaveBeenCalledTimes(2);
    expect(openVpnSignIn).toHaveBeenCalledOnce();
  });
});


it("reports shared-container cleanup as pending and can retry without closing or restarting sign-in", async () => {
  const message = "The VPN connection was released, but its shared container is still running.";
  vi.mocked(cancelVpnEnrollment).mockRejectedValueOnce({ code: "vpn_cleanup_pending", message }).mockResolvedValueOnce(undefined);
  const { result, on_close, on_connection_id } = setup();
  await act(async () => { await result.current.begin(input); });
  await act(async () => { await result.current.cancel(); });
  expect(result.current.cleanup_pending).toBe(true);
  expect(result.current.error).toBe(message);
  expect(on_close).not.toHaveBeenCalled();
  expect(on_connection_id).not.toHaveBeenCalledWith(null);
  await act(async () => { await result.current.cancel(); });
  expect(cancelVpnEnrollment).toHaveBeenCalledTimes(2);
  expect(beginVpnEnrollment).toHaveBeenCalledOnce();
  expect(result.current.cleanup_pending).toBe(false);
  expect(on_close).toHaveBeenCalledOnce();
});


it("does not allow saving a login whose retained container status is unavailable", async () => {
  vi.mocked(beginVpnEnrollment).mockResolvedValue({ ...connected, status: { ...connected.status, status_unavailable: true } });
  const { result, on_save } = setup();
  await act(async () => { await result.current.begin(input); });
  expect(result.current.ready).toBe(false);
  await act(async () => { expect(await result.current.save()).toBe(false); });
  expect(on_save).not.toHaveBeenCalled();
});
