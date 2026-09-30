// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NotificationBell, Notifications } from "./Notifications";
import { NotificationStore } from "../../features/notifications/NotificationStore";
import { CommandProvider } from "../../features/commands/CommandContext";
import { CommandDispatcher } from "../../features/commands/CommandDispatcher";
import { QUICK_INPUT_IDS } from "../../features/commands/commandIds";
import type { NotificationSeverity } from "../../lib/types";

afterEach(() => { cleanup(); vi.useRealTimers(); });

function setup(severity: NotificationSeverity = "error") {
  const store = new NotificationStore();
  const dispatcher = new CommandDispatcher();
  dispatcher.update([], true, vi.fn());
  const view = (blocked = false) => <CommandProvider value={{ dispatcher, keybinding: () => undefined }}>
    <input aria-label="Terminal input" />
    <NotificationBell store={store} />
    <Notifications store={store} blocked={blocked} />
  </CommandProvider>;
  const rendered = render(view());
  const terminal = screen.getByRole("textbox", { name: "Terminal input" });
  terminal.focus();
  const report = () => store.report("host", {
    severity, title: "Connection", message: "Host status", source: "workstation",
    actions: [{ label: "Retry", command_id: "retry" }],
  });
  act(report);
  return { store, dispatcher, terminal, report, rendered, view };
}

describe("notifications", () => {
  it("does not steal focus, hides cards for review, and dismisses them from history", () => {
    const { store, terminal, report } = setup();
    expect(document.activeElement).toBe(terminal);
    fireEvent.click(screen.getByRole("button", { name: "Hide Connection notification" }));
    expect(screen.queryByRole("article")).toBeNull();
    const bell = screen.getByRole("button", { name: "Notifications, 1 unread" });
    bell.focus();
    fireEvent.click(bell);
    const center = screen.getByRole("region", { name: "Notification center" });
    expect(document.activeElement).toBe(center);
    expect(within(center).getByText("Host status")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Notifications" })).toBeTruthy();
    const dismiss = within(center).getByRole("button", { name: "Dismiss Connection notification" });
    dismiss.focus();
    fireEvent.click(dismiss);
    expect(document.activeElement).toBe(center);
    expect(screen.getByText("No notifications")).toBeTruthy();
    act(report);
    expect(store.snapshot().entries).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Hide notification center" }));
    expect(document.activeElement).toBe(bell);
  });

  it("closes the center on Escape and keeps other app commands available", () => {
    const { store, dispatcher, terminal } = setup();
    const run = vi.fn();
    act(() => dispatcher.update([{ id: "other", title: "Other", category: "Test", enabled: true, run }], true, vi.fn()));
    act(() => store.setCenterOpen(true));
    expect(dispatcher.canExecute("other")).toBe(true);
    act(() => { dispatcher.execute(QUICK_INPUT_IDS.cancel); });
    expect(screen.queryByRole("region")).toBeNull();
    expect(document.activeElement).toBe(terminal);
  });

  it("auto-hides informational cards after eight seconds of visible, unfocused time", () => {
    vi.useFakeTimers();
    const { store } = setup("info");
    act(() => vi.advanceTimersByTime(4000));
    fireEvent.mouseEnter(screen.getByRole("article"));
    act(() => vi.advanceTimersByTime(20000));
    expect(screen.getByRole("article")).toBeTruthy();
    fireEvent.mouseLeave(screen.getByRole("article"));
    const hide = screen.getByRole("button", { name: "Hide Connection notification" });
    act(() => hide.focus());
    act(() => vi.advanceTimersByTime(20000));
    expect(screen.getByRole("article")).toBeTruthy();
    act(() => hide.blur());
    act(() => vi.advanceTimersByTime(3999));
    expect(screen.getByRole("article")).toBeTruthy();
    act(() => vi.advanceTimersByTime(1));
    expect(screen.queryByRole("article")).toBeNull();
    expect(store.snapshot().entries).toMatchObject([{ toast_visible: false, read: false }]);
  });

  it("keeps error cards until hidden or dismissed and preserves keyboard focus on close", () => {
    vi.useFakeTimers();
    const { store } = setup();
    act(() => vi.advanceTimersByTime(60000));
    const dismiss = screen.getByRole("button", { name: "Dismiss Connection notification" });
    act(() => dismiss.focus());
    fireEvent.click(dismiss);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Notifications" }));
    expect(store.snapshot().entries).toHaveLength(0);
  });

  it("uses current command availability and hides accepted actions without deleting history", () => {
    const { store, dispatcher } = setup();
    expect((screen.getByRole("button", { name: "Retry" }) as HTMLButtonElement).disabled).toBe(true);
    const run = vi.fn();
    act(() => dispatcher.update([{ id: "retry", title: "Retry", category: "Test", enabled: true, run }], true, vi.fn()));
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(run).toHaveBeenCalledOnce();
    expect(store.snapshot().entries).toMatchObject([{ toast_visible: false }]);
    act(() => store.setCenterOpen(true));
    act(() => dispatcher.update([], true, vi.fn()));
    expect((screen.getByRole("button", { name: "Retry" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("gets out of the way of modal dialogs without dropping history", () => {
    const { store, rendered, view } = setup();
    act(() => store.setCenterOpen(true));
    rendered.rerender(view(true));
    expect(screen.queryByRole("region")).toBeNull();
    expect(store.snapshot().center_open).toBe(false);
    expect(store.snapshot().entries).toHaveLength(1);
    rendered.rerender(view());
    fireEvent.click(screen.getByRole("button", { name: "Notifications" }));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss all notifications" }));
    expect(screen.getByText("No notifications")).toBeTruthy();
  });
});
