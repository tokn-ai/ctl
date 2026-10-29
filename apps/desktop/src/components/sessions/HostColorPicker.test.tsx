// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HOST_COLOR_PALETTE } from "../../features/workspace/hostColor";
import { HostColorPicker } from "./HostColorPicker";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function picker(props: Partial<Parameters<typeof HostColorPicker>[0]> = {}) {
  return <HostColorPicker
    host_name="Build"
    color={HOST_COLOR_PALETTE[0].color}
    default_color="hsl(120 64% 68%)"
    on_change={vi.fn()}
    {...props}
  />;
}

describe("HostColorPicker", () => {
  it("changes connection fill without remounting or interrupting color selection", async () => {
    const user = userEvent.setup();
    const on_change = vi.fn();
    const { rerender } = render(picker({ on_change }));
    const trigger = screen.getByRole("button", { name: "Change color for Build" });
    const dot = trigger.querySelector<HTMLElement>(".host-color-dot")!;
    const original_color = dot.style.color;
    expect(dot.style.background).toBe("currentcolor");
    expect(trigger.getAttribute("aria-description")).toBe("Connected");

    await user.click(trigger);
    const dialog = screen.getByRole("dialog", { name: "Color for Build" });
    const reset_dot = within(dialog).getByRole("button", { name: "Reset" })
      .querySelector<HTMLElement>(".host-color-dot")!;
    const reset_background = reset_dot.style.background;
    rerender(picker({ on_change, connected: false }));
    expect(screen.getByRole("dialog", { name: "Color for Build" })).toBe(dialog);
    expect(screen.getByRole("button", { name: "Change color for Build" })).toBe(trigger);
    expect(trigger.querySelector(".host-color-dot")).toBe(dot);
    expect(trigger.getAttribute("data-connected")).toBe("false");
    expect(trigger.getAttribute("aria-description")).toBe("Not connected");
    expect(dot.style.background).toBe("transparent");
    expect(dot.style.color).toBe(original_color);
    expect(reset_dot.style.background).toBe(reset_background);

    rerender(picker({ on_change, connected: true }));
    expect(trigger.getAttribute("data-connected")).toBe("true");
    expect(dot.style.background).toBe("currentcolor");
    expect(dot.style.color).toBe(original_color);
    await user.click(screen.getByRole("button", { name: "Blue" }));
    expect(on_change).toHaveBeenCalledExactlyOnceWith(HOST_COLOR_PALETTE.find(({ name }) => name === "Blue")?.color);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("portals out of clipping containers and selects a named palette color", async () => {
    const user = userEvent.setup();
    const on_change = vi.fn();
    const { container } = render(<div style={{ overflow: "hidden" }}>{picker({ on_change })}</div>);
    const trigger = screen.getByRole("button", { name: "Change color for Build" });
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(trigger);
    const dialog = screen.getByRole("dialog", { name: "Color for Build" });
    expect(container.contains(dialog)).toBe(false);
    expect(dialog.parentElement).toBe(document.body);
    expect(dialog.querySelector("button button")).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Coral" }));
    expect(screen.getByRole("button", { name: "Coral" }).getAttribute("aria-pressed")).toBe("true");
    await user.click(screen.getByRole("button", { name: "Blue" }));
    expect(on_change).toHaveBeenCalledWith(HOST_COLOR_PALETTE.find(({ name }) => name === "Blue")?.color);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("keeps random suggestions stable until shuffled and can select one", async () => {
    const user = userEvent.setup();
    const random = vi.spyOn(Math, "random").mockReturnValue(0.1);
    const on_change = vi.fn();
    const { rerender } = render(picker({ on_change }));
    await user.click(screen.getByRole("button", { name: "Change color for Build" }));
    const suggestions = () => within(screen.getByRole("group", { name: "Random colors" }))
      .getAllByRole("button").map((button) => button.getAttribute("aria-label"));
    const initial = suggestions();
    expect(initial).toHaveLength(4);
    random.mockReturnValue(0.8);
    rerender(picker({ on_change }));
    expect(suggestions()).toEqual(initial);
    await user.click(screen.getByRole("button", { name: "Shuffle" }));
    expect(suggestions()).not.toEqual(initial);
    const selected = within(screen.getByRole("group", { name: "Random colors" })).getAllByRole("button")[0];
    const color = selected.getAttribute("aria-label")?.replace("Color ", "");
    await user.click(selected);
    expect(on_change).toHaveBeenCalledWith(color);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("resets the preference and closes", async () => {
    const user = userEvent.setup();
    const on_change = vi.fn();
    render(picker({ on_change }));
    await user.click(screen.getByRole("button", { name: "Change color for Build" }));
    await user.click(screen.getByRole("button", { name: "Reset" }));
    expect(on_change).toHaveBeenCalledExactlyOnceWith(undefined);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("opens with the keyboard and returns focus on Escape without changing color", async () => {
    const user = userEvent.setup();
    const on_change = vi.fn();
    render(picker({ on_change }));
    const trigger = screen.getByRole("button", { name: "Change color for Build" });
    await user.tab();
    expect(document.activeElement).toBe(trigger);
    await user.keyboard("{Enter}");
    expect(screen.getByRole("dialog")).toBeTruthy();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);
    expect(on_change).not.toHaveBeenCalled();
  });

  it("dismisses outside without swallowing the next action", async () => {
    const user = userEvent.setup();
    const on_change = vi.fn();
    const outside = vi.fn();
    render(<>{picker({ on_change })}<button onClick={outside}>Other action</button></>);
    await user.click(screen.getByRole("button", { name: "Change color for Build" }));
    await user.click(screen.getByRole("button", { name: "Other action" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(outside).toHaveBeenCalledOnce();
    expect(on_change).not.toHaveBeenCalled();
  });

  it("keeps disabled hosts fixed and closes if disabled while open", async () => {
    const user = userEvent.setup();
    const { rerender } = render(picker({ disabled: true, color: "#ffffff" }));
    const trigger = screen.getByRole("button", { name: "Change color for Build" });
    expect((trigger as HTMLButtonElement).disabled).toBe(true);
    await user.click(trigger);
    expect(screen.queryByRole("dialog")).toBeNull();
    rerender(picker());
    await user.click(trigger);
    expect(screen.getByRole("dialog")).toBeTruthy();
    rerender(picker({ disabled: true }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
  });
});
