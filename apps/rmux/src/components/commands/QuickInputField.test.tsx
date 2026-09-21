// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { QuickInput } from "./QuickInput";
import type { QuickInputFieldMode } from "./QuickInputField";

afterEach(cleanup);

const mode: QuickInputFieldMode = {
  kind: "input",
  label: "Identity file",
  suggestions: {
    label: "Identity files in ~/.ssh",
    items: [
      { id: "/test-home/.ssh/id_ed25519", label: "~/.ssh/id_ed25519" },
      { id: "/test-home/.ssh/local.id_rsa", label: "~/.ssh/local.id_rsa" },
    ],
    empty_message: "No identities found. Enter a path manually.",
  },
};

describe("quick-input suggestions", () => {
  it("shows accessible groups for picker choices without adding keyboard stops or reordering options", async () => {
    const submit = vi.fn();
    render(<QuickInput title="Connect host" mode={{
      kind: "pick",
      choices: [
        { id: "local", label: "Local" },
        { id: "saved-build", label: "Build machine", group: "Saved hosts" },
        { id: "virtual-office", label: "office", group: "SSH config · Virtual" },
      ],
    }} onSubmit={submit} onCancel={vi.fn()} />);
    const saved = screen.getByRole("group", { name: "Saved hosts" });
    const virtual = screen.getByRole("group", { name: "SSH config · Virtual" });
    expect(within(saved).getByText("Saved hosts")).toBeTruthy();
    expect(within(virtual).getByText("SSH config · Virtual")).toBeTruthy();
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "Local", "Build machine", "office",
    ]);
    expect(document.activeElement).toBe(screen.getByRole("option", { name: "Local" }));
    const user = userEvent.setup();
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(within(saved).getByRole("option", { name: "Build machine" }));
    await user.keyboard("{ArrowDown}{Enter}");
    expect(document.activeElement).toBe(within(virtual).getByRole("option", { name: "office" }));
    expect(submit).toHaveBeenCalledExactlyOnceWith("virtual-office");
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(screen.getByRole("option", { name: "Local" }));
  });

  it("filters out empty suggestion groups while retaining flat navigation and the editable draft", async () => {
    const submit = vi.fn();
    render(<QuickInput title="Add host" mode={{
      kind: "input",
      label: "SSH host",
      suggestions: {
        label: "Hosts",
        items: [
          { id: "saved-build", label: "build", group: "Saved hosts" },
          { id: "ssh-config:office", label: "office", group: "SSH config · Virtual" },
          { id: "ssh-config:office-vpn", label: "office-vpn", group: "SSH config · Virtual" },
        ],
      },
    }} onSubmit={submit} onCancel={vi.fn()} />);
    const input = screen.getByRole("combobox", { name: "SSH host" });
    const last = screen.getByRole("option", { name: "office-vpn" });
    const scroll = vi.fn();
    last.scrollIntoView = scroll;
    const user = userEvent.setup();
    await user.keyboard("{ArrowUp}");
    expect(input.getAttribute("aria-activedescendant")).toBe(last.id);
    expect(scroll).toHaveBeenCalledExactlyOnceWith({ block: "nearest" });
    expect(document.activeElement).toBe(input);
    await user.type(input, "office");
    expect(screen.queryByRole("group", { name: "Saved hosts" })).toBeNull();
    expect(within(screen.getByRole("group", { name: "SSH config · Virtual" })).getAllByRole("option")).toHaveLength(2);
    expect(input.getAttribute("aria-activedescendant")).toBeNull();
    await user.keyboard("{ArrowDown}{ArrowUp}");
    expect(input).toHaveProperty("value", "office");
    expect(screen.getByRole("option", { name: "office-vpn" }).getAttribute("aria-selected")).toBe("true");
    await user.keyboard("{Enter}");
    expect(submit).toHaveBeenLastCalledWith("ssh-config:office-vpn");
    await user.clear(input);
    expect(screen.getByRole("group", { name: "Saved hosts" })).toBeTruthy();
    await user.type(input, "manual.example{Enter}");
    expect(screen.queryAllByRole("group")).toHaveLength(0);
    expect(screen.queryAllByRole("option")).toHaveLength(0);
    expect(submit).toHaveBeenLastCalledWith("manual.example");
  });

  it("filters suggestions and selects with arrows and Enter while retaining input focus", async () => {
    const submit = vi.fn();
    render(
      <QuickInput
        title="Identity"
        mode={mode}
        onSubmit={submit}
        onCancel={vi.fn()}
      />,
    );
    const user = userEvent.setup();
    const input = screen.getByRole("combobox");
    await user.type(input, "LOCAL");
    expect(screen.getAllByRole("option")).toHaveLength(1);
    await user.keyboard("{ArrowDown}");
    const option = screen.getByRole("option", { name: "~/.ssh/local.id_rsa" });
    expect(option.getAttribute("aria-selected")).toBe("true");
    expect(input.getAttribute("aria-activedescendant")).toBe(option.id);
    expect(document.activeElement).toBe(input);
    await user.keyboard("{Enter}");
    expect(submit).toHaveBeenCalledExactlyOnceWith(
      "/test-home/.ssh/local.id_rsa",
    );
  });

  it("selects a file by click and wraps keyboard selection", async () => {
    const submit = vi.fn();
    render(
      <QuickInput
        title="Identity"
        mode={mode}
        onSubmit={submit}
        onCancel={vi.fn()}
      />,
    );
    const user = userEvent.setup();
    await user.keyboard("{ArrowUp}{ArrowDown}{Enter}");
    expect(submit).toHaveBeenLastCalledWith("/test-home/.ssh/id_ed25519");
    await user.click(
      screen.getByRole("option", { name: "~/.ssh/local.id_rsa" }),
    );
    expect(submit).toHaveBeenLastCalledWith("/test-home/.ssh/local.id_rsa");
  });

  it("never substitutes a suggestion for a typed manual path without selection", async () => {
    const submit = vi.fn();
    render(
      <QuickInput
        title="Identity"
        mode={mode}
        onSubmit={submit}
        onCancel={vi.fn()}
      />,
    );
    const user = userEvent.setup();
    const input = screen.getByRole("combobox");
    await user.keyboard("{ArrowDown}");
    await user.type(input, "/another/key{Enter}");
    expect(submit).toHaveBeenCalledExactlyOnceWith("/another/key");
    expect(screen.queryAllByRole("option")).toHaveLength(0);
  });

  it("keeps the draft editable while suggestions load and does not select on arrival", async () => {
    const submit = vi.fn();
    const { rerender } = render(
      <QuickInput
        title="Identity"
        mode={{
          ...mode,
          suggestions: { ...mode.suggestions!, items: [], loading: true },
        }}
        onSubmit={submit}
        onCancel={vi.fn()}
      />,
    );
    const user = userEvent.setup();
    await user.type(screen.getByRole("combobox"), "local");
    expect(screen.getByRole("status").textContent).toBe("Loading suggestions…");
    rerender(
      <QuickInput
        title="Identity"
        mode={mode}
        onSubmit={submit}
        onCancel={vi.fn()}
      />,
    );
    expect((screen.getByRole("combobox") as HTMLInputElement).value).toBe(
      "local",
    );
    expect(
      screen.getByRole("combobox").getAttribute("aria-activedescendant"),
    ).toBeNull();
    await user.keyboard("{Enter}");
    expect(submit).toHaveBeenCalledExactlyOnceWith("local");
  });

  it("does not consume Enter from Cancel as a suggestion choice", async () => {
    const submit = vi.fn();
    const cancel = vi.fn();
    render(
      <QuickInput
        title="Identity"
        mode={mode}
        onSubmit={submit}
        onCancel={cancel}
      />,
    );
    const user = userEvent.setup();
    await user.keyboard("{ArrowDown}");
    await user.tab({ shift: true });
    await user.keyboard("{Enter}");
    expect(cancel).toHaveBeenCalledOnce();
    expect(submit).not.toHaveBeenCalled();
  });

  it("uses the supplied list label for host suggestions too", async () => {
    const submit = vi.fn();
    render(
      <QuickInput
        title="Host"
        mode={{
          kind: "input",
          label: "Host",
          suggestions: {
            label: "SSH config hosts",
            items: [{ id: "ssh-config:rmux-test", label: "rmux-test" }],
          },
        }}
        onSubmit={submit}
        onCancel={vi.fn()}
      />,
    );
    expect(
      screen.getByRole("listbox", { name: "SSH config hosts" }),
    ).toBeTruthy();
    await userEvent.setup().keyboard("{ArrowDown}{Enter}");
    expect(submit).toHaveBeenCalledExactlyOnceWith("ssh-config:rmux-test");
  });
});
