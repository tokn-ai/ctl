// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { WorkspaceSidebar } from "./WorkspaceSidebar";

afterEach(cleanup);

describe("workspace sidebar", () => {
  it("shows current VPN activity even when another panel is selected", () => {
    const panel = render(
      <WorkspaceSidebar
        selected="sessions" onSelect={vi.fn()}
        sessions={null} tasks={null} ports={null} vpn={null}
        vpn_state="connected"
      />,
    );
    const tab = screen.getByRole("button", { name: "VPN" });
    expect(tab.getAttribute("aria-pressed")).toBe("false");
    expect(tab.getAttribute("aria-description")).toBe("Connected");
    expect(tab.querySelector(".vpn-activity-indicator.connected")).toBeTruthy();
    panel.rerender(
      <WorkspaceSidebar
        selected="sessions" onSelect={vi.fn()}
        sessions={null} tasks={null} ports={null} vpn={null}
        vpn_state="connected" vpn_status_stale
      />,
    );
    expect(tab.getAttribute("aria-description")).toBe("Status unavailable");
    expect(tab.querySelector(".vpn-activity-indicator.connected")).toBeNull();
    panel.rerender(
      <WorkspaceSidebar
        selected="sessions" onSelect={vi.fn()}
        sessions={null} tasks={null} ports={null} vpn={null}
        vpn_state="stopped"
      />,
    );
    expect(tab.getAttribute("aria-description")).toBe("Disconnected");
    expect(tab.querySelector(".vpn-activity-indicator")).toBeNull();
  });

  it("reports the number of active VPNs and marks stale counts unavailable", () => {
    const props = {
      selected: "sessions" as const, onSelect: vi.fn(),
      sessions: null, tasks: null, ports: null, vpn: null, error: null,
      vpn_state: "connected" as const, vpn_active_count: 2,
    };
    const panel = render(<WorkspaceSidebar {...props} />);
    const tab = screen.getByRole("button", { name: "VPN" });
    expect(tab.getAttribute("aria-description")).toBe("2 active VPNs");
    panel.rerender(<WorkspaceSidebar {...props} vpn_status_stale />);
    expect(tab.getAttribute("aria-description")).toBe("Status unavailable");
  });

  it("does not claim disconnection for incomplete inventory while preserving confirmed activity", () => {
    const props = {
      selected: "sessions" as const, onSelect: vi.fn(),
      sessions: null, tasks: null, ports: null, vpn: null, vpn_inventory_incomplete: true,
    };
    const panel = render(<WorkspaceSidebar {...props} vpn_state="stopped" vpn_active_count={0} />);
    const tab = screen.getByRole("button", { name: "VPN" });
    expect(tab.getAttribute("aria-description")).toBe("Status unavailable");
    expect(tab.querySelector(".vpn-activity-indicator.stale")).toBeTruthy();
    panel.rerender(<WorkspaceSidebar {...props} vpn_state="connected" vpn_active_count={1} />);
    expect(tab.getAttribute("aria-description")).toBe("Connected · Inventory incomplete");
    expect(tab.querySelector(".vpn-activity-indicator.connected")).toBeTruthy();
    panel.rerender(<WorkspaceSidebar {...props} vpn_state="connected" vpn_active_count={2} />);
    expect(tab.getAttribute("aria-description")).toBe("2 active VPNs · Inventory incomplete");
  });

  it("selects the ports panel from workspace navigation", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <WorkspaceSidebar
        selected="sessions"
        onSelect={onSelect}
        sessions={<span>Sessions panel</span>} tasks={null}
        ports={<span>Ports panel</span>}
        vpn={<span>VPN panel</span>}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Ports" }));
    expect(onSelect).toHaveBeenCalledWith("ports");
  });

  it("switches the sidebar to tasks and keeps the selected destination on repeated clicks", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<WorkspaceSidebar selected="tasks" onSelect={onSelect}
      sessions={<button>Open local shell</button>} tasks={<span>Task definitions</span>} ports={null} vpn={null} running_task_count={2} />);
    expect(screen.queryByRole("button", { name: "Open local shell" })).toBeNull();
    expect(screen.getByText("Task definitions")).toBeTruthy();
    const tasks = screen.getByRole("button", { name: "Tasks" });
    expect(tasks.getAttribute("aria-pressed")).toBe("true");
    expect(tasks.title).toContain("2 running");
    await user.click(tasks);
    expect(onSelect).toHaveBeenCalledWith("tasks");
  });
});
