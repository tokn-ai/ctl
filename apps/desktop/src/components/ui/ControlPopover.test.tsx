// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ControlPopover } from "./ControlPopover";

afterEach(cleanup);

describe("control popover", () => {
  it("closes a navigation menu after invoking a labeled action", async () => {
    const user = userEvent.setup();
    const action = vi.fn();
    render(<ControlPopover label="Settings" placement="right" close_on_select><button onClick={action}>Credentials</button></ControlPopover>);
    expect(screen.queryByRole("button", { name: "Credentials" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Settings" }));
    await user.click(screen.getByRole("button", { name: "Credentials" }));
    expect(action).toHaveBeenCalledOnce();
    expect(screen.queryByRole("group", { name: "Settings" })).toBeNull();
  });

  it("keeps actions out of the tab order until opened, and returns focus on Escape", async () => {
    const user = userEvent.setup();
    const action = vi.fn();
    render(<ControlPopover label="Layout"><button onClick={action}>Release resize</button></ControlPopover>);
    expect(screen.queryByRole("button", { name: "Release resize" })).toBeNull();
    const trigger = screen.getByRole("button", { name: "Layout" });
    await user.click(trigger);
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Release resize" }));
    await user.keyboard("{Escape}");
    expect(document.activeElement).toBe(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(action).not.toHaveBeenCalled();
  });

  it("dismisses on outside focus without consuming that interaction", async () => {
    const user = userEvent.setup();
    const action = vi.fn();
    render(<><ControlPopover label="Layout"><button>Resize</button></ControlPopover><button onClick={action}>Other action</button></>);
    await user.click(screen.getByRole("button", { name: "Layout" }));
    await user.click(screen.getByRole("button", { name: "Other action" }));
    expect(screen.queryByRole("group", { name: "Layout" })).toBeNull();
    expect(action).toHaveBeenCalledOnce();
  });
});
