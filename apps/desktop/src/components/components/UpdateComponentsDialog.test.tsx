// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UpdateComponentsDialog } from "./UpdateComponentsDialog";
import { cancelComponentUpdate, checkComponentUpdates, componentUpdateCheckoutAvailable, respondSshPrompt, updateComponents } from "../../lib/tauri";
import type { ComponentUpdateHostResult, ComponentUpdateProgress, SshConnectionTarget } from "../../lib/types";

vi.mock("../../lib/tauri", () => ({ checkComponentUpdates: vi.fn(), componentUpdateCheckoutAvailable: vi.fn(), updateComponents: vi.fn(), cancelComponentUpdate: vi.fn(async () => undefined), respondSshPrompt: vi.fn(async () => undefined) }));
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(cancelComponentUpdate).mockResolvedValue(undefined);
  vi.mocked(respondSshPrompt).mockResolvedValue(undefined);
  vi.mocked(checkComponentUpdates).mockResolvedValue({ default_channel: "nightly", local_development: false, catalog: null, error: null });
  vi.mocked(componentUpdateCheckoutAvailable).mockResolvedValue(false);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllEnvs(); });
const host: SshConnectionTarget = { kind: "ssh", host_id: "builder", host_name: "Builder", destination: "builder-alias", gateways: [{ kind: "ssh", gateway_id: "jump", name: "Jump", destination: "jump", mode: "automatic" }] };
function complete(host_index: number, remote_archive_reused?: boolean): ComponentUpdateHostResult {
  return {
    host_index, state: "complete", error: null,
    result: { package: "full_bundle", bundle_id: "verified-build", target_triple: "aarch64-apple-darwin", services_preserved: true, remote_archive_reused },
  };
}

it("prefills supported hosts and installs a complete build from supplied files", async () => {
  vi.mocked(updateComponents).mockResolvedValue([complete(0), complete(1), complete(2)]);
  const windows: SshConnectionTarget = { kind: "ssh", destination: "win", remote_platform: "windows" };
  render(<UpdateComponentsDialog targets={[host, windows]} context={{ targets: [host, windows] }} on_close={vi.fn()} />);
  const user = userEvent.setup();
  expect((screen.getByRole("checkbox", { name: "Builder" }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: "local" }) as HTMLInputElement).checked).toBe(false);
  expect(screen.getByRole("checkbox", { name: "win" })).toHaveProperty("checked", true);
  expect(screen.queryAllByRole("radio")).toHaveLength(0);
  await user.click(screen.getByRole("checkbox", { name: "local" }));
  expect(screen.getByText(/the host tries curl, then wget/)).toBeTruthy();
  await user.selectOptions(screen.getByRole("combobox", { name: "Build source" }), "provided");
  expect(screen.getByText(/This computer prepares the files for each SSH host/)).toBeTruthy();
  expect(screen.queryByText(/the host tries curl, then wget/)).toBeNull();
  await user.type(screen.getByRole("textbox", { name: "Build directory or archive" }), "/tmp/build directory");
  await user.click(screen.getByRole("button", { name: "Update" }));
  await screen.findByText("3 of 3 hosts updated. Running services were preserved.");
  expect(updateComponents).toHaveBeenCalledExactlyOnceWith({ targets: [{ kind: "local" }, host, windows], attempt_id: expect.any(String), options: { package: "full_bundle", source: { kind: "provided", path: "/tmp/build directory", local_build: false, ctld_package: null } } }, expect.any(Function), expect.any(Function));
});

it("shows only Windows SSH routes on a Windows desktop and never substitutes the preferred route", () => {
  vi.spyOn(navigator, "platform", "get").mockReturnValue("Win32");
  const windows: SshConnectionTarget = { ...host, host_id: "win", destination: "primary", remote_platform: "windows" };
  const explicit: SshConnectionTarget = { ...windows, destination: "alternate", remote_platform: "unix" };
  const view = render(<UpdateComponentsDialog targets={[windows, host]} context={{ targets: [explicit] }} on_close={vi.fn()} />);
  expect(screen.queryAllByRole("checkbox")).toHaveLength(0);
  expect(screen.getByRole("button", { name: "Update" })).toHaveProperty("disabled", true);
  expect(updateComponents).not.toHaveBeenCalled();
  view.unmount();

  render(<UpdateComponentsDialog targets={[host, windows]} context={{ targets: [windows] }} on_close={vi.fn()} />);
  expect(screen.getAllByRole("checkbox")).toHaveLength(1);
  expect(screen.getByRole("checkbox", { name: "Builder" })).toHaveProperty("checked", true);
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
  expect(vi.mocked(updateComponents).mock.calls[0][0].options.source).toEqual({ kind: "automatic" });
});

it.each(["stable", "nightly"] as const)("keeps an explicit %s source and installs its published bundle", async (channel) => {
  vi.mocked(updateComponents).mockResolvedValue([complete(0)]);
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [host], source: { kind: "published", channel } }} on_close={vi.fn()} />);
  const user = userEvent.setup();
  expect((screen.getByRole("combobox", { name: "Build source" }) as HTMLSelectElement).value).toBe(channel);
  await waitFor(() => expect(checkComponentUpdates).toHaveBeenCalledWith(channel, false));
  await user.click(screen.getByRole("button", { name: "Update" }));
  await screen.findByText("1 of 1 hosts updated. Running services were preserved.");
  expect(vi.mocked(updateComponents).mock.calls[0][0].options.source).toEqual({ kind: "published", channel });
  expect(screen.getByText(/Bundle verified-build/)).toBeTruthy();
});

it("offers Nightly as the main build choice and sends it to discovery and installation", async () => {
  vi.mocked(updateComponents).mockResolvedValue([complete(0)]);
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [host] }} on_close={vi.fn()} />);
  const user = userEvent.setup();
  const select = screen.getByRole("combobox", { name: "Build source" }) as HTMLSelectElement;
  expect(select.value).toBe("automatic");
  expect(checkComponentUpdates).not.toHaveBeenCalled();
  expect(Array.from(select.options, (option) => option.value)).not.toContain("dev");
  await user.selectOptions(select, screen.getByRole("option", { name: "Nightly (latest published main build)" }));
  await waitFor(() => expect(checkComponentUpdates).toHaveBeenLastCalledWith("nightly", false));
  await user.click(screen.getByRole("button", { name: "Update" }));
  await screen.findByText("1 of 1 hosts updated. Running services were preserved.");
  expect(vi.mocked(updateComponents).mock.calls[0][0].options.source).toEqual({ kind: "published", channel: "nightly" });
});

it("avoids a single channel preview when automatic sources depend on each machine", async () => {
  vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
  render(<UpdateComponentsDialog targets={[host]} on_close={vi.fn()} />);
  const user = userEvent.setup();
  const mac_hint = /This Mac uses its active signed component build or a verified cached copy of that build/;
  expect(screen.getByText(mac_hint)).toBeTruthy();
  expect(screen.queryByRole("region", { name: "Published component bundles" })).toBeNull();
  expect(checkComponentUpdates).not.toHaveBeenCalled();

  await user.click(screen.getByRole("checkbox", { name: "Builder" }));
  expect(screen.getByText(mac_hint)).toBeTruthy();
  expect(screen.queryByRole("region", { name: "Published component bundles" })).toBeNull();
  expect(checkComponentUpdates).not.toHaveBeenCalled();

  await user.click(screen.getByRole("checkbox", { name: "Builder" }));
  expect(screen.queryByRole("region", { name: "Published component bundles" })).toBeNull();
});

it("keeps an explicit offline install available without a network check", async () => {
  vi.mocked(updateComponents).mockResolvedValue([complete(0)]);
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [host], source: { kind: "selected" } }} on_close={vi.fn()} />);
  expect(checkComponentUpdates).not.toHaveBeenCalled();
  await userEvent.setup().click(screen.getByRole("button", { name: "Update" }));
  await screen.findByText("1 of 1 hosts updated. Running services were preserved.");
  expect(vi.mocked(updateComponents).mock.calls[0][0].options.source).toEqual({ kind: "selected" });
});

it("offers the installed app's checkout for a remote build independently of published bundles", async () => {
  vi.stubEnv("DEV", false);
  vi.mocked(componentUpdateCheckoutAvailable).mockResolvedValue(true);
  vi.mocked(updateComponents).mockResolvedValue([complete(0)]);
  const mac: SshConnectionTarget = { ...host, host_id: "mac", host_name: "Mac", remote_info: {
    remote_id: "mac-account", agent_version: "test",
    bundle: { app_version: "test", bundle_id: "test", git_revision: "test", target_triple: "aarch64-apple-darwin" },
  } };
  render(<UpdateComponentsDialog targets={[host, mac]} context={{ targets: [host], source: { kind: "selected" } }} on_close={vi.fn()} />);
  const user = userEvent.setup();
  const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Build source" });
  await user.selectOptions(select, await screen.findByRole("option", { name: "Build from checkout" }));
  expect(screen.getByText(/upload them over SSH for Linux x64 or ARM64/)).toBeTruthy();
  expect(screen.getByText(/This computer prepares the files for each SSH host/)).toBeTruthy();
  expect(screen.queryByText(/the host tries curl, then wget/)).toBeNull();
  const update = screen.getByRole<HTMLButtonElement>("button", { name: "Update" });
  for (const name of ["local", "Mac"]) {
    const incompatible = screen.getByRole("checkbox", { name });
    await user.click(incompatible);
    expect(select.value).toBe("working_tree");
    expect(update.disabled).toBe(true);
    expect(screen.getByText(/Choose only Linux x64 or ARM64 SSH hosts/)).toBeTruthy();
    await user.click(incompatible);
  }
  expect(update.disabled).toBe(false);
  await user.click(update);
  expect(vi.mocked(updateComponents).mock.calls[0][0]).toMatchObject({ targets: [host], options: { source: { kind: "working_tree" } } });
  expect(checkComponentUpdates).not.toHaveBeenCalled();
});

it("keeps a preselected checkout build blocked when its local capability check fails", async () => {
  vi.mocked(componentUpdateCheckoutAvailable).mockRejectedValue(new Error("Checkout missing"));
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [host], source: { kind: "working_tree" } }} on_close={vi.fn()} />);
  expect(await screen.findByText(/The local source checkout is unavailable/)).toBeTruthy();
  const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Build source" });
  expect(select.value).toBe("working_tree");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Update" }).disabled).toBe(true);
  await userEvent.setup().selectOptions(select, "provided");
  expect(screen.queryByRole("option", { name: "Build from checkout" })).toBeNull();
  expect(updateComponents).not.toHaveBeenCalled();
  expect(checkComponentUpdates).not.toHaveBeenCalled();
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
  act(() => report({ host_index: 1, state: "updating", progress: { phase: "downloading_bundle", file_name: "ctlup", transferred_bytes: 1024, total_bytes: 4096, bytes_per_second: 0 } }));
  expect(screen.getByText("Downloading ctlup to this computer…")).toBeTruthy();
  expect(screen.getByRole("progressbar", { name: "Downloading ctlup to this computer…" }).getAttribute("value")).toBe("1024");
  act(() => report({ host_index: 1, state: "updating", progress: { phase: "building_bundle", file_name: "Compiling ctl-agent v0.1.2", transferred_bytes: 0, total_bytes: 0, bytes_per_second: 0 } }));
  expect(screen.getByText("Compiling ctl-agent v0.1.2")).toBeTruthy();
  expect(screen.getByRole("progressbar", { name: "Building components on this computer…" }).hasAttribute("value")).toBe(false);
  act(() => report({ host_index: 1, state: "updating", progress: { phase: "transferring", file_name: "ctl-agent", transferred_bytes: 2048, total_bytes: 8192, bytes_per_second: 1024 } }));
  expect(screen.getByRole("progressbar").getAttribute("value")).toBe("2048");
  expect(screen.getByText("2 KiB / 8 KiB · 25% · 1 KiB/s")).toBeTruthy();
});

it("identifies the local cache, remote fetch, and SSH fallback as distinct actions for each host", async () => {
  let report!: (progress: ComponentUpdateProgress) => void;
  vi.mocked(updateComponents).mockImplementation((_request, _prompt, onProgress) => {
    report = onProgress;
    return new Promise(() => undefined);
  });
  const other: SshConnectionTarget = { ...host, host_id: "second", host_name: "Second" };
  render(<UpdateComponentsDialog targets={[host, other]} context={{ targets: [host, other] }} on_close={vi.fn()} />);
  await userEvent.setup().click(screen.getByRole("button", { name: "Update" }));
  const counts = { file_name: null, transferred_bytes: 0, total_bytes: 0, bytes_per_second: 0 };
  const first_host = [
    ["checking_local_cache", "Checking this computer’s cache for the selected release…"],
    ["upload_cached_updater", "Uploading cached ctlup over SSH…"],
    ["remote_install_runtime", "Installing the runtime with remote ctlup, which downloads it on the host if needed…"],
    ["upload_fallback", "The host could not install the release directly. Switching to SSH upload…"],
  ] as const;
  for (const [phase, action] of first_host) {
    act(() => report({ host_index: 0, state: "updating", progress: { ...counts, phase } }));
    expect(screen.getByRole("progressbar", { name: action }).hasAttribute("value")).toBe(false);
    if (phase === "upload_cached_updater") {
      act(() => report({ host_index: 0, state: "updating", progress: { ...counts, phase, transferred_bytes: 8, total_bytes: 16 } }));
      expect(screen.getByRole("progressbar", { name: action }).getAttribute("value")).toBe("8");
      expect(screen.getByText("8 B / 16 B · 50%")).toBeTruthy();
    } else {
      expect(screen.queryByText("8 B / 16 B · 50%")).toBeNull();
    }
  }
  act(() => report({ host_index: 0, state: "updating", progress: { ...counts, phase: "transferring", ssh_upload_fallback: true } }));
  expect(screen.getByRole("progressbar", { name: "Uploading the runtime bundle over SSH…" })).toBeTruthy();
  expect(screen.getByText(/This update switched to SSH upload/)).toBeTruthy();
  const second_host = [
    ["checking_local_cache", "Checking this computer’s cache for the selected release…"],
    ["remote_download_updater", "Downloading ctlup on the remote host…"],
    ["upload_cached_runtime", "Uploading the cached runtime bundle over SSH…"],
  ] as const;
  for (const [phase, action] of second_host) {
    act(() => report({ host_index: 1, state: "updating", progress: { ...counts, phase } }));
    expect(screen.getByRole("progressbar", { name: action }).hasAttribute("value")).toBe(false);
    expect(screen.queryByText(/This update switched to SSH upload/)).toBeNull();
  }
  act(() => report({ host_index: 1, state: "updating", progress: { ...counts, phase: "upload_cached_runtime", transferred_bytes: 2048, total_bytes: 8192 } }));
  expect(screen.getByRole("progressbar", { name: "Uploading the cached runtime bundle over SSH…" }).getAttribute("max")).toBe("8192");
  expect(screen.getByText("2 KiB / 8 KiB · 25%")).toBeTruthy();
});

it("shows when the remote ctlup activates an existing build and keeps that outcome in the results", async () => {
  let report!: (progress: ComponentUpdateProgress) => void;
  let finish!: (results: ComponentUpdateHostResult[]) => void;
  vi.mocked(updateComponents).mockImplementation((_request, _prompt, onProgress) => {
    report = onProgress;
    return new Promise((resolve) => { finish = resolve; });
  });
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [host] }} on_close={vi.fn()} />);
  await userEvent.setup().click(screen.getByRole("button", { name: "Update" }));
  const counts = { file_name: null, transferred_bytes: 0, total_bytes: 0, bytes_per_second: 0 };
  act(() => report({ host_index: 0, state: "updating", progress: { ...counts, phase: "checking_remote_installation" } }));
  expect(screen.getByText("Checking ctlup and installed components on the remote host…")).toBeTruthy();
  act(() => report({ host_index: 0, state: "updating", progress: { ...counts, phase: "reused" } }));
  expect(screen.getByText("Activated the build already on the remote host.")).toBeTruthy();
  expect(screen.queryByRole("progressbar")).toBeNull();
  act(() => report({ host_index: 0, state: "complete", progress: null }));
  await act(async () => finish([complete(0, true)]));
  expect(screen.getByText("Activated existing build")).toBeTruthy();
});

it("identifies the reused remote build from the host result without any progress messages", async () => {
  vi.mocked(updateComponents).mockResolvedValue([complete(0), complete(1, true)]);
  render(<UpdateComponentsDialog targets={[host]} context={{ targets: [{ kind: "local" }, host] }} on_close={vi.fn()} />);
  await userEvent.setup().click(screen.getByRole("button", { name: "Update" }));
  expect((await screen.findByText("Activated existing build")).closest("li")?.textContent).toContain("Builder");
  expect(screen.getByText("Installed").closest("li")?.textContent).toContain("local");
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
  const input = screen.getByRole("textbox", { name: "Build directory or archive" });
  expect(input.getAttribute("aria-invalid")).toBe("true");
  expect(document.getElementById(input.getAttribute("aria-describedby")!)?.contains(screen.getByRole("alert"))).toBe(true);
  expect(updateComponents).not.toHaveBeenCalled();
});
