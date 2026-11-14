// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { NotificationBell, Notifications } from "../../components/notifications/Notifications";
import { WorkspaceSidebar } from "../../components/workspace/WorkspaceSidebar";
import { CommandProvider } from "../commands/CommandContext";
import { CommandDispatcher } from "../commands/CommandDispatcher";
import { NotificationStore } from "../notifications/NotificationStore";
import { LocaleProvider, resolveLocale } from "./LocaleContext";
import { message } from "./messages";

afterEach(() => {
  cleanup();
  localStorage.clear();
  document.documentElement.lang = "";
});

describe("desktop display language", () => {
  it("follows ordered system languages and honors an explicit setting", () => {
    expect(resolveLocale("system", ["fr-CA", "zh-CN", "en-US"])).toBe("zh-Hans");
    expect(resolveLocale("system", ["zh-TW", "zh-SG"])).toBe("zh-Hans");
    expect(resolveLocale("system", ["zh-Hant", "de-DE"])).toBe("en");
    expect(resolveLocale("system", ["invalid_tag", "en-GB", "zh-CN"])).toBe("en");
    expect(resolveLocale("en", ["zh-CN"])).toBe("en");
  });

  it("changes existing notifications live and remembers the selection for the next window", () => {
    const store = new NotificationStore();
    const dispatcher = new CommandDispatcher();
    const task_notification = {
      severity: "error" as const,
      title: message("notifications.task_failed"),
      source: "Example build",
      message: message("notifications.task_failed_with_exit", { name: "Example build", exit_code: 12 }),
      actions: [{ label: message("notifications.view_task"), command_id: "task.open" }],
    };
    store.report("task", task_notification);
    store.report("recovered", { ...task_notification, message: "verbatim daemon output" });
    store.resolve("recovered");
    store.report("dismissed", { ...task_notification, message: "dismissed details" });
    const dismissed = store.snapshot().entries.find((entry) => entry.source_key === "dismissed")!;
    store.dismiss(dismissed.id);
    store.setCenterOpen(true);
    const original = store.snapshot();

    const app = () => <LocaleProvider>
      <WorkspaceSidebar selected="sessions" onSelect={() => {}} sessions={null} tasks={null} ports={null} vpn={null} />
      <CommandProvider value={{ dispatcher, keybinding: () => undefined }}>
        <NotificationBell store={store} />
        <Notifications store={store} blocked={false} />
      </CommandProvider>
    </LocaleProvider>;

    const view = render(app());
    expect(screen.getByRole("region", { name: "Notification center" })).toBeTruthy();
    expect(screen.getByText("Example build failed with exit code 12.")).toBeTruthy();
    expect(screen.getByText("Resolved")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const language = screen.getByRole("combobox", { name: "Language" });
    language.focus();
    fireEvent.change(language, { target: { value: "zh-Hans" } });

    const center = screen.getByRole("region", { name: "通知中心" });
    expect(within(center).getByText("Example build 失败，退出代码为 12。")).toBeTruthy();
    expect(within(center).getByRole("button", { name: "查看任务" })).toBeTruthy();
    expect(within(center).getByText("已解决")).toBeTruthy();
    expect(within(center).getByText("verbatim daemon output")).toBeTruthy();
    expect(within(center).queryByText("dismissed details")).toBeNull();
    expect(document.activeElement).toBe(language);
    expect(document.documentElement.lang).toBe("zh-Hans");
    expect(store.snapshot()).toBe(original);

    view.unmount();
    render(app());
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    expect((screen.getByRole("combobox", { name: "语言" }) as HTMLSelectElement).value).toBe("zh-Hans");

    act(() => {
      localStorage.setItem("ctmux.desktop.language", "en");
      window.dispatchEvent(new StorageEvent("storage", { key: "ctmux.desktop.language" }));
    });
    expect(screen.getByRole("region", { name: "Notification center" })).toBeTruthy();
  });
});
