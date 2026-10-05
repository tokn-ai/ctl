// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ComponentProtocolVersion, ComponentVersionInfo, ComponentVersionRow } from "../../lib/types";
import { ComponentVersionTable } from "./ComponentVersionTable";

const protocol = (name: string, version: string, supported_versions = [version]): ComponentProtocolVersion => ({ name, version, build: Number(version.split(".")[2]), supported_versions });
const info: ComponentVersionInfo = { version: "0.1.0", source_revision: "abcdef123456", source_fingerprint: "same-build", dirty: false, protocols: [protocol("ctld", "1.0.12")] };
const unreported: ComponentVersionInfo = { ...info, version: null, source_revision: null, source_fingerprint: null, protocols: [] };
const row: ComponentVersionRow = {
  component_id: "remote:saved:dev:ctld", component: "ctld", label: "ctld — Development — SSH", location: "remote", host_id: "dev",
  host_key: "saved:dev", host_name: "Development — SSH", observation: "running", status: "current", running: info, installed: info, available: info,
  required_protocols: info.protocols, restart_supported: true, action: "restart", detail: null, error: null,
};
const props = () => ({ busy_id: null, restarting: false, action_error: null, on_restart: vi.fn() });
const expand = () => fireEvent.click(screen.getByRole("button", { name: "Show components for Development — SSH" }));
afterEach(cleanup);

it("starts collapsed, summarizes outdated hosts, and exposes host actions once for the whole group", () => {
  const manage = vi.fn();
  render(<ComponentVersionTable {...props()} rows={[{ ...row, status: "outdated" }, { ...row, component_id: "agent", component: "ctl_agent", action: null }]} manageable_host_ids={["dev"]} on_manage_host={manage} />);
  expect(screen.getByRole("button", { name: "Show components for Development — SSH" }).getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByRole("table", { name: "Development — SSH component versions" })).toBeNull();
  const hosts = within(screen.getByRole("table", { name: "Remote component hosts" }));
  expect(hosts.getByRole("columnheader", { name: "Host" })).toBeTruthy();
  expect(hosts.getByRole("columnheader", { name: "Status" })).toBeTruthy();
  expect(hosts.getByRole("columnheader", { name: "Actions" })).toBeTruthy();
  expect(within(screen.getByRole("button", { name: "Show components for Development — SSH" }).closest("tr")!).getByText("Outdated")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Check host Development — SSH" }));
  expect(manage).toHaveBeenCalledWith("dev", "inspect");
  fireEvent.click(screen.getByRole("button", { name: "Update components Development — SSH" }));
  expect(manage).toHaveBeenCalledWith("dev", "update");
  expand();
  const table = screen.getByRole("table", { name: "Development — SSH component versions" });
  expect(within(table).getByRole("rowheader", { name: "ctld" })).toBeTruthy();
  expect(within(table).getByRole("rowheader", { name: "ctl-agent" })).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Update components Development — SSH" })).toHaveLength(1);
});

it("checks each protocol independently against explicit app contracts, including missing reports", () => {
  const actual = [protocol("ctld", "1.0.12"), protocol("ctld_lifecycle", "1.1.3", ["1.0.1", "1.1.3"]), protocol("ctld_helper", "1.1.2")];
  const required = [protocol("ctld", "1.0.12"), protocol("ctld_lifecycle", "1.0.1"), protocol("ctld_helper", "1.0.1"), protocol("task", "1.0.1")];
  render(<ComponentVersionTable {...props()} rows={[{ ...row, running: { ...info, protocols: actual }, installed: { ...info, protocols: actual }, required_protocols: required }]} />);
  expand();
  const protocols = within(screen.getByRole("list", { name: "Running + on disk protocols" }));
  expect(protocols.getAllByRole("listitem")).toHaveLength(4);
  expect(protocols.getByLabelText("ctld IPC 1.0.12: Current").className).toContain("about-protocol-current");
  expect(protocols.getByLabelText("Lifecycle 1.1.3: Compatible").className).toContain("about-protocol-compatible");
  expect(protocols.getByLabelText("Helper API 1.1.2: Incompatible").className).toContain("about-protocol-incompatible");
  expect(protocols.getByLabelText("Task Not reported: Unverified").className).toContain("about-protocol-unknown");
  expect(protocols.getByLabelText("Lifecycle 1.1.3: Compatible").title).toContain("Requires 1.0.1");
});

it("shows both disk and running when fingerprints differ even with the same version and revision", () => {
  render(<ComponentVersionTable {...props()} rows={[{ ...row, restart_required: true, running: { ...info, source_fingerprint: "old-build" }, installed: { ...info, source_fingerprint: "new-build" } }]} />);
  expand();
  const table = within(screen.getByRole("table", { name: "Development — SSH component versions" }));
  expect(table.getByText("Running")).toBeTruthy();
  expect(table.getByText("On disk")).toBeTruthy();
  expect(table.getByText("Build old-build")).toBeTruthy();
  expect(table.getByText("Build new-build")).toBeTruthy();
  expect(table.getAllByRole("row")).toHaveLength(3);
  expect(table.getAllByRole("button", { name: "Restart ctld — Development — SSH" })).toHaveLength(1);
});

it("collapses matching verified builds regardless of protocol ordering", () => {
  const protocols = [protocol("ctld", "1.1.13", ["1.0.12", "1.1.13"]), protocol("ctld_lifecycle", "1.0.1")];
  render(<ComponentVersionTable {...props()} rows={[{ ...row, running: { ...info, protocols }, installed: { ...info, protocols: [...protocols].reverse().map((entry) => ({ ...entry, supported_versions: [...entry.supported_versions].reverse() })) } }]} />);
  expand();
  const table = within(screen.getByRole("table", { name: "Development — SSH component versions" }));
  expect(table.getByText("Running + on disk")).toBeTruthy();
  expect(table.getAllByRole("row")).toHaveLength(2);
});

it("does not use a remote app reference as an installation, and shows identical reported builds once", () => {
  const unknown = { ...info, source_revision: null, source_fingerprint: null };
  const view = render(<ComponentVersionTable {...props()} rows={[{ ...row, status: "unknown", running: unknown, installed: undefined }]} />);
  expand();
  expect(within(screen.getByRole("table", { name: "Development — SSH component versions" })).queryByTitle(/Build: same-build/)).toBeNull();
  expect(within(screen.getByRole("table", { name: "Development — SSH component versions" })).getByText("Unknown")).toBeTruthy();
  expect(within(screen.getByRole("button", { name: "Hide components for Development — SSH" }).closest("tr")!).getByText("Unverified")).toBeTruthy();
  view.rerender(<ComponentVersionTable {...props()} rows={[{ ...row, status: "unknown", running: unknown, installed: unknown }]} />);
  expect(within(screen.getByRole("table", { name: "Development — SSH component versions" })).getByText("Running + on disk")).toBeTruthy();
  expect(within(screen.getByRole("table", { name: "Development — SSH component versions" })).getAllByText("0.1.0")).toHaveLength(1);
  const toggle = within(screen.getByRole("button", { name: "Hide components for Development — SSH" }).closest("tr")!);
  expect(toggle.getByText("Unverified")).toBeTruthy();
  expect(toggle.queryByText("Running differs")).toBeNull();
});

it("retains expansion on refresh and keeps identically named unsaved accounts separate", () => {
  const view = render(<ComponentVersionTable {...props()} rows={[row]} />);
  expand();
  view.rerender(<ComponentVersionTable {...props()} rows={[{ ...row, status: "outdated" }]} />);
  expect(screen.getByRole("button", { name: "Hide components for Development — SSH" }).getAttribute("aria-expanded")).toBe("true");
  view.rerender(<ComponentVersionTable {...props()} rows={[{ ...row, host_id: null, host_key: "account:first" }, { ...row, component_id: "second", host_id: null, host_key: "account:second" }]} />);
  expect(screen.getAllByRole("button", { name: "Show components for Development — SSH" })).toHaveLength(2);
  expect(screen.queryByRole("table", { name: "Development — SSH component versions" })).toBeNull();
});

it("shows only installed contracts when a legacy running build is unknown", () => {
  render(<ComponentVersionTable {...props()} rows={[{ ...row, running: null, observation: "legacy", status: "incompatible", legacy_protocols: [{ name: "ctld", version: 12 }], restart_required: true }]} />);
  expand();
  expect(screen.queryByRole("list", { name: "Running protocols" })).toBeNull();
  expect(within(screen.getByRole("list", { name: "On disk protocols" })).getByLabelText("ctld IPC 1.0.12: Current")).toBeTruthy();
});

it("keeps owner-specific restart actions and disables management during an action", () => {
  const options = props();
  const manage = vi.fn();
  const view = render(<ComponentVersionTable {...options} rows={[row]} manageable_host_ids={["dev"]} on_manage_host={manage} />);
  expand();
  fireEvent.click(screen.getByRole("button", { name: "Restart ctld — Development — SSH" }));
  expect(options.on_restart).toHaveBeenCalledWith(row.component_id);
  view.rerender(<ComponentVersionTable {...options} busy_id={row.component_id} restarting rows={[row]} manageable_host_ids={["dev"]} on_manage_host={manage} />);
  expect((screen.getByRole("button", { name: "Check host Development — SSH" }) as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Restart ctld — Development — SSH" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText("Restarting…")).toBeTruthy();
});

it("keeps local components open while allowing only one remote host open", () => {
  const local = { ...row, component_id: "local", location: "local" as const, host_id: null, host_key: null, host_name: null, label: "ctld (SSH)" };
  const second = { ...row, component_id: "second", host_id: "second", host_key: "saved:second", host_name: "Second host" };
  render(<ComponentVersionTable {...props()} rows={[local, row, second]} />);
  expect(screen.getByRole("table", { name: "This computer component versions" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: /components for This computer/ })).toBeNull();
  expand();
  expect(screen.getByRole("table", { name: "Development — SSH component versions" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Show components for Second host" }));
  expect(screen.queryByRole("table", { name: "Development — SSH component versions" })).toBeNull();
  expect(screen.getByRole("table", { name: "Second host component versions" })).toBeTruthy();
  expect(screen.getByRole("table", { name: "This computer component versions" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Hide components for Second host" }));
  expect(screen.queryByRole("table", { name: "Second host component versions" })).toBeNull();
  expect(screen.getByRole("table", { name: "This computer component versions" })).toBeTruthy();
});

it("shows shared SSH failures once per host with a short message and retained diagnostic", () => {
  const error = "Could not inspect companions or running owners. Check the connection and component inspection support before updating. unexpected end of file";
  const rows = ["ctl_agent", "ctmuxd", "ctl-taskd"].map((component) => ({ ...row, component_id: component, component: component as ComponentVersionRow["component"], observation: "not_checked" as const, status: "unavailable" as const, running: null, installed: null, action: null, error }));
  render(<ComponentVersionTable {...props()} rows={rows} manageable_host_ids={["dev"]} on_manage_host={vi.fn()} />);
  expect(screen.getAllByRole("alert")).toHaveLength(1);
  expect(screen.getByRole("alert").textContent).toBe("Could not check components. Reconnect and try again.");
  expect(screen.getByRole("alert").title).toBe(error);
  expand();
  expect(screen.getAllByRole("alert")).toHaveLength(1);
  expect(screen.getAllByTitle(error)).toHaveLength(1);
  expect(screen.queryByText(error)).toBeNull();
  expect(within(screen.getByRole("table", { name: "Development — SSH component versions" })).queryByText(/^Running/)).toBeNull();
});

it.each([
  ["absent", null, "unknown"],
  ["unreported", unreported, "unknown"],
  ["stopped", info, "not_running"],
] as const)("hides the %s running entry and keeps the on-disk build", (_label, running, status) => {
  render(<ComponentVersionTable {...props()} rows={[{ ...row, running, status }]} />);
  expand();
  const table = within(screen.getByRole("table", { name: "Development — SSH component versions" }));
  expect(table.queryByText(/^Running/)).toBeNull();
  expect(table.getByText("On disk")).toBeTruthy();
  expect(table.getByText("Build same-build")).toBeTruthy();
  expect(table.getAllByRole("row")).toHaveLength(2);
});
