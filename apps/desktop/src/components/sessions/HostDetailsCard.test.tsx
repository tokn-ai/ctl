// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostDetailsCard } from "./HostDetailsCard";

let anchor_bounds: DOMRect;
const details = [
  { label: "Address", value: "build.example.com:22" },
  { label: "Connection", value: "Direct SSH" },
];

beforeEach(() => {
  vi.useFakeTimers();
  anchor_bounds = new DOMRect(24, 100, 180, 28);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.hasAttribute("data-host-trigger")) return anchor_bounds;
    if (this.classList.contains("host-details-card")) return new DOMRect(0, 0, 300, 180);
    if (this.classList.contains("host-group-header")) return new DOMRect(8, 100, 232, 28);
    if (this.hasAttribute("data-clip")) return new DOMRect(0, 0, 240, 600);
    return new DOMRect(0, 0, 1024, 768);
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function card(show_trigger = true, static_trigger = false) {
  return <HostDetailsCard
    host_name="Build" label="SSH connected" tone="normal"
    description="An SSH connection is open; responsiveness has not been checked." details={details}
  >
    {(trigger_props) => show_trigger && (static_trigger
      ? <div {...trigger_props} data-host-trigger tabIndex={0}>Build</div>
      : <button {...trigger_props} data-host-trigger type="button">Build</button>)}
  </HostDetailsCard>;
}

function hover() {
  fireEvent.pointerOver(screen.getByText("Build"));
  act(() => vi.advanceTimersByTime(250));
  return screen.getByRole("tooltip");
}

describe("HostDetailsCard", () => {
  it("delays pointer hover without taking focus, and portals beside the existing trigger", () => {
    const { container } = render(<div data-clip style={{ overflowY: "auto" }}>{card()}</div>);
    const trigger = screen.getByRole("button", { name: "Build" });
    expect(container.firstElementChild?.childElementCount).toBe(1);
    fireEvent.pointerOver(trigger);
    act(() => vi.advanceTimersByTime(249));
    expect(screen.queryByRole("tooltip")).toBeNull();
    act(() => vi.advanceTimersByTime(1));
    const tooltip = screen.getByRole("tooltip");
    expect(tooltip.parentElement).toBe(document.body);
    expect(container.contains(tooltip)).toBe(false);
    expect(tooltip.style.left).toBe("212px");
    expect(tooltip.style.top).toBe("100px");
    expect(document.activeElement).toBe(document.body);
    expect(trigger.getAttribute("aria-describedby")).toBe(tooltip.id);
    expect(within(tooltip).getByText("build.example.com:22")).toBeTruthy();
    expect(within(tooltip).getByText("An SSH connection is open; responsiveness has not been checked.").tagName).toBe("P");
    expect(tooltip.querySelector("button, input, a, [tabindex]")).toBeNull();
  });

  it("bridges the pointer gap and stays open while hovering the details", () => {
    render(card());
    const trigger = screen.getByRole("button", { name: "Build" });
    const tooltip = hover();
    fireEvent.pointerOut(trigger, { relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(100));
    fireEvent.pointerOver(tooltip);
    act(() => vi.advanceTimersByTime(500));
    expect(screen.getByRole("tooltip")).toBe(tooltip);
    fireEvent.pointerOut(tooltip, { relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(150));
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("places details beyond the complete host header so its actions stay exposed", () => {
    render(<div className="host-group-header">
      {card()}<button>Connect</button><button>Settings</button>
    </div>);
    const tooltip = hover();
    expect(tooltip.style.left).toBe("248px");
    expect(Number.parseInt(tooltip.style.left)).toBeGreaterThan(anchor_bounds.right);
  });

  it("opens immediately for a focused static host and closes when focus moves to color controls", () => {
    render(<>{card(true, true)}<button>Change color</button></>);
    const trigger = screen.getByText("Build");
    act(() => trigger.focus());
    expect(screen.getByRole("tooltip")).toBeTruthy();
    expect(document.activeElement).toBe(trigger);
    const color_button = screen.getByRole("button", { name: "Change color" });
    act(() => color_button.focus());
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(document.activeElement).toBe(color_button);
    expect(trigger.hasAttribute("aria-describedby")).toBe(false);
  });

  it("allows a new hover after an outside click during the pointer leave delay", () => {
    render(<>{card()}<button>Other action</button></>);
    const trigger = screen.getByRole("button", { name: "Build" });
    hover();
    fireEvent.pointerOut(trigger, { relatedTarget: document.body });
    fireEvent.pointerDown(screen.getByRole("button", { name: "Other action" }));
    expect(screen.queryByRole("tooltip")).toBeNull();
    fireEvent.pointerOver(trigger);
    act(() => vi.advanceTimersByTime(250));
    expect(screen.getByRole("tooltip")).toBeTruthy();
  });

  it("keeps Escape dismissed until the pointer leaves or the host is refocused", () => {
    render(card());
    const trigger = screen.getByRole("button", { name: "Build" });
    hover();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("tooltip")).toBeNull();
    fireEvent.pointerOver(trigger);
    act(() => vi.advanceTimersByTime(500));
    expect(screen.queryByRole("tooltip")).toBeNull();
    fireEvent.pointerOut(trigger, { relatedTarget: document.body });
    fireEvent.pointerOver(trigger);
    act(() => vi.advanceTimersByTime(250));
    expect(screen.getByRole("tooltip")).toBeTruthy();
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(screen.queryByRole("tooltip")).toBeNull();
    act(() => trigger.focus());
    expect(screen.getByRole("tooltip")).toBeTruthy();
    expect(document.activeElement).toBe(trigger);
  });

  it("flips to the left and clamps vertically near viewport edges", () => {
    anchor_bounds = new DOMRect(900, 700, 100, 28);
    render(card());
    const tooltip = hover();
    expect(tooltip.style.left).toBe("592px");
    expect(tooltip.style.top).toBe("580px");
  });

  it.each([
    ["viewport", new DOMRect(24, -100, 180, 28)],
    ["scroll container", new DOMRect(24, 620, 180, 28)],
  ])("closes when scrolling moves the host outside its %s", (_, next_bounds) => {
    render(<div data-clip style={{ overflowY: "auto" }}>{card()}</div>);
    hover();
    anchor_bounds = next_bounds;
    fireEvent.scroll(window);
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("removes the tooltip when its trigger unmounts and cancels delayed opens on unmount", () => {
    const { rerender, unmount } = render(card());
    hover();
    rerender(card(false));
    expect(screen.queryByRole("tooltip")).toBeNull();
    rerender(card());
    fireEvent.pointerOver(screen.getByRole("button", { name: "Build" }));
    unmount();
    expect(vi.getTimerCount()).toBe(0);
    act(() => vi.advanceTimersByTime(300));
    expect(screen.queryByRole("tooltip")).toBeNull();
  });
});
