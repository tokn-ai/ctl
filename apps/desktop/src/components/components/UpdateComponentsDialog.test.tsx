// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UpdateComponentsDialog } from "./UpdateComponentsDialog";
import { cancelComponentUpdate, respondSshPrompt, updateComponents } from "../../lib/tauri";
import type { ComponentUpdateHostResult, ComponentUpdateProgress, SshConnectionTarget } from "../../lib/types";

vi.mock("../../lib/tauri", () => ({ updateComponents: vi.fn(), cancelComponentUpdate: vi.fn(async () => undefined), respondSshPrompt: vi.fn(async () => undefined) }));
beforeEach(() => { vi.resetAllMocks(); vi.mocked(cancelComponentUpdate).mockResolvedValue(undefined); vi.mocked(respondSshPrompt).mockResolvedValue(undefined); });
afterEach(cleanup);
const host: SshConnectionTarget = { kind: "ssh", host_id: "builder", host_name: "Builder", destination: "builder-alias", gateways: [{ kind: "ssh", gateway_id: "jump", name: "Jump", destination: "jump", mode: "automatic" }] };
const complete = (host_index: number): ComponentUpdateHostResult => ({ host_index, state: "complete", error: null, result: { package: "full_bundle", bundle_id: "verified-build", target_triple: "aarch64-apple-darwin", services_preserved: true } });

it("prefills a host, permits several hosts, and sends the chosen package and supplied build", async () => {
  vi.mocked(updateComponents).mockResolvedValue([complete(0), complete(1)]);
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [host] }} on_close={vi.fn()} />);
  const user = userEvent.setup();
  expect((screen.getByRole("checkbox", { name: "Builder" }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: "local" }) as HTMLInputElement).checked).toBe(false);
  expect(screen.getAllByRole("radio")).toHaveLength(2);
  await user.click(screen.getByRole("checkbox", { name: "local" }));
  await user.click(screen.getByRole("radio", { name: /ctl-agent only/ }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Build source" }), "provided");
  await user.type(screen.getByRole("textbox", { name: "Build directory or archive" }), "/tmp/build directory");
  await user.click(screen.getByRole("button", { name: "Update" }));
  await screen.findByText("2 of 2 hosts updated. Running services were preserved.");
  expect(updateComponents).toHaveBeenCalledExactlyOnceWith({ targets: [{ kind: "local" }, host], attempt_id: expect.any(String), options: { package: "ctl_agent", source: { kind: "provided", path: "/tmp/build directory", local_build: false, ctld_package: null } } }, expect.any(Function), expect.any(Function));
});

it("keeps per-host failures and retries only failed hosts", async () => {
  vi.mocked(updateComponents).mockResolvedValueOnce([complete(0), { host_index: 1, state: "failed", error: "SSH authentication failed.", result: null }]).mockResolvedValueOnce([complete(0)]);
  const updated = vi.fn();
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [{ kind: "local" }, host] }} on_updated={updated} on_close={vi.fn()} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Update" }));
  await screen.findByText("SSH authentication failed.");
  await user.click(screen.getByRole("button", { name: "Retry failed hosts…" }));
  expect((screen.getByRole("checkbox", { name: "local" }) as HTMLInputElement).checked).toBe(false);
  await user.click(screen.getByRole("button", { name: "Update" }));
  await waitFor(() => expect(updated).toHaveBeenCalledTimes(2));
  expect(vi.mocked(updateComponents).mock.calls[1][0].targets).toEqual([host]);
});

it("binds authentication responses to the active host attempt and reports transfer progress", async () => {
  let report!: (progress: ComponentUpdateProgress) => void;
  vi.mocked(updateComponents).mockImplementation((_request, onPrompt, onProgress) => {
    report = onProgress;
    onProgress({ host_index: 1, state: "updating", progress: null });
    onPrompt({ prompt_id: "password", kind: "secret", message: "Password for Builder", warning: null });
    return new Promise(() => undefined);
  });
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [{ kind: "local" }, host] }} on_close={vi.fn()} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Update" }));
  await user.type(await screen.findByLabelText("SSH response"), "test secret{Enter}");
  const attempt = vi.mocked(updateComponents).mock.calls[0][0].attempt_id;
  expect(respondSshPrompt).toHaveBeenCalledWith(`${attempt}:1`, "password", "test secret");
  act(() => report({ host_index: 1, state: "updating", progress: { phase: "transferring", file_name: "ctl-agent", transferred_bytes: 2048, total_bytes: 8192, bytes_per_second: 1024 } }));
  expect(screen.getByRole("progressbar").getAttribute("value")).toBe("2048");
  expect(screen.getByText("2 KiB / 8 KiB · 25% · 1 KiB/s")).toBeTruthy();
});

it("stops a batch without closing the result view or claiming activation was undone", async () => {
  let finish!: (results: ComponentUpdateHostResult[]) => void;
  vi.mocked(updateComponents).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
  const close = vi.fn();
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [host] }} on_close={close} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Update" }));
  const attempt = vi.mocked(updateComponents).mock.calls[0][0].attempt_id;
  await user.click(screen.getByRole("button", { name: "Stop update" }));
  expect(cancelComponentUpdate).toHaveBeenCalledWith(attempt);
  expect(close).not.toHaveBeenCalled();
  await act(async () => finish([{ host_index: 0, state: "cancelled", result: null, error: "Refresh status before retrying; activation may have completed." }]));
  await screen.findByText(/activation may have completed/);
  await user.click(screen.getByRole("button", { name: "Done" }));
  expect(close).toHaveBeenCalledOnce();
});

it("requires supplied files and does not turn an unavailable saved host into a direct connection", async () => {
  const offline: SshConnectionTarget = { ...host, kind: "ssh", unavailable: "Saved route is missing." };
  render(<UpdateComponentsDialog targets={[offline]} context={{ targets: [offline], source: { kind: "provided", path: "", local_build: false, ctld_package: null } }} on_close={vi.fn()} />);
  const user = userEvent.setup();
  expect((screen.getByRole("checkbox", { name: /Builder/ }) as HTMLInputElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Update" }) as HTMLButtonElement).disabled).toBe(true);
  await user.click(screen.getByRole("checkbox", { name: "local" }));
  await user.click(screen.getByRole("button", { name: "Update" }));
  expect(screen.getByRole("alert").textContent).toBe("Enter a build directory or archive path.");
  expect(updateComponents).not.toHaveBeenCalled();
});
