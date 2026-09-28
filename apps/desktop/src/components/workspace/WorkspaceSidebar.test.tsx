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
        sessions={null} tasks={null} ports={null} vpn={null} error={null}
        vpn_state="connected"
      />,
    );
    const tab = screen.getByRole("tab", { name: "VPN" });
    expect(tab.getAttribute("aria-selected")).toBe("false");
    expect(tab.getAttribute("aria-description")).toBe("Connected");
    expect(tab.querySelector(".vpn-activity-indicator.connected")).toBeTruthy();
    panel.rerender(
      <WorkspaceSidebar
        selected="sessions" onSelect={vi.fn()}
        sessions={null} tasks={null} ports={null} vpn={null} error={null}
        vpn_state="connected" vpn_status_stale
      />,
    );
    expect(tab.getAttribute("aria-description")).toBe("Status unavailable");
    expect(tab.querySelector(".vpn-activity-indicator.connected")).toBeNull();
    panel.rerender(
      <WorkspaceSidebar
        selected="sessions" onSelect={vi.fn()}
        sessions={null} tasks={null} ports={null} vpn={null} error={null}
        vpn_state="stopped"
      />,
    );
    expect(tab.getAttribute("aria-description")).toBe("Disconnected");
    expect(tab.querySelector(".vpn-activity-indicator")).toBeNull();
  });

  it("selects the ports panel from the activity rail", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <WorkspaceSidebar
        selected="sessions"
        onSelect={onSelect}
        sessions={<span>Sessions panel</span>}
        tasks={<span>Tasks panel</span>}
        ports={<span>Ports panel</span>}
        vpn={<span>VPN panel</span>}
        error={null}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Ports" }));
    expect(onSelect).toHaveBeenCalledWith("ports");
  });

  it("moves through all four tabs with vertical arrow keys", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <WorkspaceSidebar
        selected="tasks"
        onSelect={onSelect}
        sessions={null}
        tasks={null}
        ports={null}
        vpn={null}
        error={null}
      />,
    );

    const tasks = screen.getByRole("tab", { name: "Tasks" });
    tasks.focus();
    await user.keyboard("{ArrowDown}");
    expect(onSelect).toHaveBeenCalledWith("ports");
    await user.keyboard("{ArrowDown}");
    expect(onSelect).toHaveBeenLastCalledWith("vpn");
    await user.keyboard("{ArrowDown}");
    expect(onSelect).toHaveBeenLastCalledWith("sessions");
    await user.keyboard("{End}");
    expect(onSelect).toHaveBeenLastCalledWith("vpn");
  });
});
