// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  PortForwardStatus,
  SshConnectionTarget,
  WorkspacePortForward,
} from "../../lib/types";
import { PortForwardingSidebar } from "./PortForwardingSidebar";

const targets: SshConnectionTarget[] = [
  { kind: "ssh", host_id: "alpha", destination: "alpha.example" },
  { kind: "ssh", host_id: "beta", destination: "beta.example" },
];

const forwards: WorkspacePortForward[] = [
  {
    forward_id: "database",
    host_id: "alpha",
    name: "Database",
    bind_address: "127.0.0.1",
    local_port: 5432,
    remote_host: "127.0.0.1",
    remote_port: 5432,
    enabled: true,
  },
  {
    forward_id: "preview",
    host_id: "beta",
    name: "Preview",
    bind_address: "127.0.0.1",
    local_port: 3000,
    remote_host: "127.0.0.1",
    remote_port: 3000,
    enabled: false,
  },
];

afterEach(cleanup);

describe("port forwarding sidebar", () => {
  it("groups saved forwards by host and controls them without hiding stopped rules", async () => {
    const user = userEvent.setup();
    const onSetEnabled = vi.fn();
    const onManage = vi.fn();
    const statuses = new Map<string, PortForwardStatus>([
      [
        "database",
        {
          forward: forwards[0],
          state: "active",
          message: null,
        },
      ],
    ]);
    render(
      <PortForwardingSidebar
        targets={targets}
        forwards={forwards}
        statuses={statuses}
        busy={new Set()}
        hostErrors={new Map()}
        refreshing={false}
        lastRefreshedAt={null}
        onRefresh={vi.fn()}
        onSetEnabled={onSetEnabled}
        onManage={onManage}
      />,
    );

    expect(screen.getByText("Database")).toBeTruthy();
    expect(screen.getByText("Preview")).toBeTruthy();
    expect(screen.getByText("Active")).toBeTruthy();
    expect(screen.getByText("Stopped")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Start" }));
    expect(onSetEnabled).toHaveBeenCalledWith(targets[1], forwards[1], true);
    await user.click(screen.getByTitle("Manage Database"));
    expect(onManage).toHaveBeenCalledWith(targets[0]);
  });

  it("shows a useful empty state", () => {
    render(
      <PortForwardingSidebar
        targets={targets}
        forwards={[]}
        statuses={new Map()}
        busy={new Set()}
        hostErrors={new Map()}
        refreshing={false}
        lastRefreshedAt={null}
        onRefresh={vi.fn()}
        onSetEnabled={vi.fn()}
        onManage={vi.fn()}
      />,
    );

    expect(screen.getByText("No saved port forwards.")).toBeTruthy();
  });
});
