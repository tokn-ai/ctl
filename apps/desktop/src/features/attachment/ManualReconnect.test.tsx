// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import type { SshConnectionStatus, SshConnectionTarget } from "../../lib/types";
import { MANUAL_RECONNECT_STATUS_TIMEOUT_MS, ManualReconnectCoordinator, ManualReconnectProvider, useManualReconnect, useManualReconnectHandler } from "./ManualReconnect";

afterEach(() => { cleanup(); vi.useRealTimers(); });

const target: SshConnectionTarget = {
  kind: "ssh", host_id: "fixture", destination: "workstation", hostname: "runtime.example.test",
  method_id: "alternate", user: "fixture", port: 2222,
  vpn_connection_id: "saved-vpn", gateways: [{ gateway_id: "jump", name: "Jump", destination: "jump.example.test", mode: "native_only" }],
};
const connected: SshConnectionStatus = { connected: true, manually_disconnected: false };
const disconnected: SshConnectionStatus = { connected: false, manually_disconnected: false };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (failure: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function setup(status: SshConnectionStatus = connected) {
  const query = vi.fn(async () => status);
  const coordinator = new ManualReconnectCoordinator(query);
  const authenticate = vi.fn(async (_target: SshConnectionTarget, _signal: AbortSignal) => true);
  const unregister = coordinator.register(authenticate);
  return { coordinator, query, authenticate, unregister };
}

describe("manual attachment reconnect preparation", () => {
  it("checks the exact route freshly and reuses its connected master", async () => {
    const { coordinator, query, authenticate } = setup();
    expect(await coordinator.request(target, new AbortController().signal)).toBe(true);
    expect(query).toHaveBeenCalledExactlyOnceWith(target);
    expect(authenticate).not.toHaveBeenCalled();
  });

  it("keeps local reconnect independent from SSH status and authentication", async () => {
    const { coordinator, query, authenticate } = setup();
    expect(await coordinator.request({ kind: "local" }, new AbortController().signal)).toBe(true);
    expect(query).not.toHaveBeenCalled();
    expect(authenticate).not.toHaveBeenCalled();
  });

  it.each([disconnected, { connected: true, manually_disconnected: true }])(
    "authenticates a missing or manually paused master before continuing", async (status) => {
      const { coordinator, authenticate } = setup(status);
      const verification = deferred<boolean>();
      authenticate.mockReturnValue(verification.promise);
      let finished = false;
      const request = coordinator.request(target, new AbortController().signal).then((result) => { finished = true; return result; });
      await vi.waitFor(() => expect(authenticate).toHaveBeenCalledExactlyOnceWith(target, expect.any(AbortSignal)));
      expect(finished).toBe(false);
      verification.resolve(true);
      expect(await request).toBe(true);
    },
  );

  it("supports batch platforms without a broker without guessing other status failures", async () => {
    const { coordinator, query, authenticate } = setup();
    query.mockRejectedValueOnce({ code: "ssh_broker_unsupported", message: "Unsupported" });
    expect(await coordinator.request(target, new AbortController().signal)).toBe(true);
    const failure = { code: "broker_unavailable", message: "Status unavailable" };
    query.mockRejectedValueOnce(failure);
    await expect(coordinator.request(target, new AbortController().signal)).rejects.toBe(failure);
    expect(authenticate).not.toHaveBeenCalled();
  });

  it("rejects an incomplete status response instead of treating it as disconnection", async () => {
    const { coordinator, query, authenticate } = setup();
    query.mockResolvedValueOnce({} as SshConnectionStatus);
    await expect(coordinator.request(target, new AbortController().signal)).rejects.toMatchObject({ code: "invalid_ssh_status" });
    expect(authenticate).not.toHaveBeenCalled();
  });

  it("bounds an unresolved status query and consumes its late result", async () => {
    vi.useFakeTimers();
    const { coordinator, query, authenticate } = setup();
    const status = deferred<SshConnectionStatus>();
    query.mockReturnValue(status.promise);
    const result = expect(coordinator.request(target, new AbortController().signal)).rejects.toMatchObject({ code: "ssh_status_timeout" });
    await vi.advanceTimersByTimeAsync(MANUAL_RECONNECT_STATUS_TIMEOUT_MS);
    await result;
    status.resolve(disconnected);
    await Promise.resolve();
    expect(authenticate).not.toHaveBeenCalled();
  });

  it("does not open authentication after cancellation during the status query", async () => {
    const { coordinator, query, authenticate } = setup();
    const status = deferred<SshConnectionStatus>();
    query.mockReturnValue(status.promise);
    const controller = new AbortController();
    const result = coordinator.request(target, controller.signal);
    controller.abort();
    expect(await result).toBe(false);
    status.resolve(disconnected);
    await Promise.resolve();
    expect(authenticate).not.toHaveBeenCalled();
  });

  it("allows a concrete authentication race to request the host flow directly", async () => {
    const { coordinator, query, authenticate } = setup();
    expect(await coordinator.request(target, new AbortController().signal, true)).toBe(true);
    expect(query).not.toHaveBeenCalled();
    expect(authenticate).toHaveBeenCalledExactlyOnceWith(target, expect.any(AbortSignal));
  });

  it("shares authentication for one exact route without cancelling another live participant", async () => {
    const { coordinator, authenticate } = setup(disconnected);
    const verification = deferred<boolean>();
    authenticate.mockReturnValue(verification.promise);
    const first = new AbortController();
    const first_result = coordinator.request(target, first.signal);
    const second_result = coordinator.request({ ...target }, new AbortController().signal);
    await vi.waitFor(() => expect(authenticate).toHaveBeenCalledOnce());
    first.abort();
    expect(await first_result).toBe(false);
    expect(authenticate.mock.calls[0][1].aborted).toBe(false);
    verification.resolve(true);
    expect(await second_result).toBe(true);
  });

  it("aborts its owned authentication when all participants cancel and ignores late success", async () => {
    const { coordinator, authenticate } = setup(disconnected);
    const verification = deferred<boolean>();
    authenticate.mockReturnValue(verification.promise);
    const controller = new AbortController();
    const result = coordinator.request(target, controller.signal);
    await vi.waitFor(() => expect(authenticate).toHaveBeenCalledOnce());
    controller.abort();
    expect(await result).toBe(false);
    expect(authenticate.mock.calls[0][1].aborted).toBe(true);
    verification.resolve(true);
    await Promise.resolve();
    authenticate.mockResolvedValue(true);
    expect(await coordinator.request(target, new AbortController().signal)).toBe(true);
    expect(authenticate).toHaveBeenCalledTimes(2);
  });

  it("does not replace a different route's active authentication", async () => {
    const { coordinator, authenticate } = setup(disconnected);
    const verification = deferred<boolean>();
    authenticate.mockReturnValue(verification.promise);
    const result = coordinator.request(target, new AbortController().signal);
    await vi.waitFor(() => expect(authenticate).toHaveBeenCalledOnce());
    await expect(coordinator.request({ ...target, port: 2200 }, new AbortController().signal))
      .rejects.toMatchObject({ code: "manual_reconnect_busy" });
    expect(authenticate).toHaveBeenCalledOnce();
    expect(authenticate.mock.calls[0][1].aborted).toBe(false);
    verification.resolve(false);
    expect(await result).toBe(false);
  });

  it("settles pending authentication when the window handler unmounts", async () => {
    const { coordinator, authenticate, unregister } = setup(disconnected);
    authenticate.mockReturnValue(new Promise(() => {}));
    const result = coordinator.request(target, new AbortController().signal);
    await vi.waitFor(() => expect(authenticate).toHaveBeenCalledOnce());
    unregister();
    expect(await result).toBe(false);
    expect(authenticate.mock.calls[0][1].aborted).toBe(true);
  });

  it("supports injected preparation and uses the latest registered host flow", async () => {
    const prepare = vi.fn(async () => true);
    const wrapper = ({ children }: { children: ReactNode }) => <ManualReconnectProvider request={prepare}>{children}</ManualReconnectProvider>;
    const hook = renderHook(useManualReconnect, { wrapper });
    const signal = new AbortController().signal;
    await act(async () => { expect(await hook.result.current?.(target, signal)).toBe(true); });
    expect(prepare).toHaveBeenCalledExactlyOnceWith(target, signal);
    hook.unmount();

    const first = vi.fn(async () => true);
    const second = vi.fn(async () => false);
    const host_hook = renderHook(({ handler }) => {
      useManualReconnectHandler(handler);
      return useManualReconnect();
    }, { wrapper: ({ children }) => <ManualReconnectProvider>{children}</ManualReconnectProvider>, initialProps: { handler: first } });
    host_hook.rerender({ handler: second });
    await act(async () => { expect(await host_hook.result.current?.(target, signal, true)).toBe(false); });
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledExactlyOnceWith(target, expect.any(AbortSignal));
  });
});
