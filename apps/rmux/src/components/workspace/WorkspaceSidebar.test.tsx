// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { WorkspaceSidebar } from "./WorkspaceSidebar";

afterEach(cleanup);

describe("workspace sidebar", () => {
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
        error={null}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Ports" }));
    expect(onSelect).toHaveBeenCalledWith("ports");
  });

  it("moves through all three tabs with vertical arrow keys", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <WorkspaceSidebar
        selected="tasks"
        onSelect={onSelect}
        sessions={null}
        tasks={null}
        ports={null}
        error={null}
      />,
    );

    const tasks = screen.getByRole("tab", { name: "Tasks" });
    tasks.focus();
    await user.keyboard("{ArrowDown}");
    expect(onSelect).toHaveBeenCalledWith("ports");
    await user.keyboard("{ArrowDown}");
    expect(onSelect).toHaveBeenLastCalledWith("sessions");
  });
});
