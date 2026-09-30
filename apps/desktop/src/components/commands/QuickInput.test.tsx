// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { QuickInput } from "./QuickInput";

afterEach(cleanup);

describe("QuickInput interactions", () => {
  it("keeps the selected gateway when asynchronous VPN choices are inserted or reordered", async () => {
    const submit = vi.fn();
    const cancel = vi.fn();
    const direct = { id: "direct", label: "Direct" };
    const gateway = { id: "gateway", label: "Office gateway", group: "Saved gateways" };
    const vpn = { id: "vpn", label: "Office VPN", group: "Saved VPNs" };
    const { rerender } = render(<QuickInput title="Connect through" mode={{ kind: "pick", choices: [direct, gateway] }}
      onSubmit={submit} onCancel={cancel} />);
    const user = userEvent.setup();
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(screen.getByRole("option", { name: "Office gateway" }));
    rerender(<QuickInput title="Connect through" mode={{ kind: "pick", choices: [direct, vpn, gateway] }}
      onSubmit={submit} onCancel={cancel} />);
    expect(screen.getByRole("option", { name: "Office gateway" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("option", { name: "Office VPN" }).getAttribute("aria-selected")).toBe("false");
    await user.keyboard("{Enter}");
    expect(submit).toHaveBeenLastCalledWith("gateway");
    rerender(<QuickInput title="Connect through" mode={{ kind: "pick", choices: [gateway, direct, vpn] }}
      onSubmit={submit} onCancel={cancel} />);
    expect(document.activeElement).toBe(screen.getByRole("option", { name: "Office gateway" }));
    await user.keyboard("{Enter}");
    expect(submit).toHaveBeenCalledTimes(2);
    expect(submit).toHaveBeenLastCalledWith("gateway");
    await user.keyboard("{ArrowDown}{Enter}");
    expect(submit).toHaveBeenLastCalledWith("direct");
  });

  it("does not substitute another choice when the selected item disappears", async () => {
    const submit = vi.fn();
    const cancel = vi.fn();
    const choices = [{ id: "direct", label: "Direct" }, { id: "vpn", label: "Office VPN" }];
    const { rerender } = render(<QuickInput title="Connect through" mode={{ kind: "pick", initial_choice_id: "vpn", choices }}
      onSubmit={submit} onCancel={cancel} />);
    rerender(<QuickInput title="Connect through" mode={{ kind: "pick", choices: [choices[0]] }}
      onSubmit={submit} onCancel={cancel} />);
    expect(screen.getByRole("option", { name: "Direct" }).getAttribute("aria-selected")).toBe("false");
    fireEvent.keyDown(screen.getByRole("listbox"), { key: "Enter" });
    expect(submit).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("option", { name: "Direct" }));
    expect(submit).toHaveBeenCalledExactlyOnceWith("direct");
  });

  it("focuses and accepts a retained choice while preserving option order", async () => {
    const submit = vi.fn();
    render(<QuickInput title="Connect through" mode={{ kind: "pick", initial_choice_id: "vpn",
      choices: [{ id: "direct", label: "Direct" }, { id: "vpn", label: "Office VPN" }] }}
      onSubmit={submit} onCancel={vi.fn()} />);
    expect(screen.getAllByRole("option")[0].textContent).toBe("Direct");
    expect(document.activeElement).toBe(screen.getByRole("option", { name: "Office VPN" }));
    await userEvent.setup().keyboard("{Enter}");
    expect(submit).toHaveBeenCalledExactlyOnceWith("vpn");
  });

  it("explicitly disables all dismissal paths for a non-cancellable operation", async () => {
    const cancel = vi.fn();
    render(
      <QuickInput
        title="Creating"
        mode={{ kind: "progress", message: "Creating shell…" }}
        cancel_disabled
        onSubmit={vi.fn()}
        onCancel={cancel}
      />,
    );
    const button = screen.getByLabelText(
      "Cancel quick input",
    ) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    const user = userEvent.setup();
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("dialog"));
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(screen.getByRole("dialog"));
    await user.keyboard("{Escape}");
    fireEvent.click(button);
    fireEvent.mouseDown(screen.getByRole("dialog").parentElement!);
    expect(cancel).not.toHaveBeenCalled();
  });

  it("captures typed values under StrictMode and submits with Enter", async () => {
    const submit = vi.fn();
    render(
      <StrictMode>
        <QuickInput
          title="Host"
          mode={{ kind: "input", label: "Host" }}
          onSubmit={submit}
          onCancel={vi.fn()}
        />
      </StrictMode>,
    );
    const user = userEvent.setup();
    await user.type(screen.getByRole("textbox"), "rmux@127.0.0.1:2222{Enter}");
    expect(submit).toHaveBeenCalledWith("rmux@127.0.0.1:2222");
  });

  it("defaults destructive confirmation to Cancel, not the destructive action", async () => {
    const submit = vi.fn();
    const cancel = vi.fn();
    render(
      <QuickInput
        title="Close"
        mode={{
          kind: "confirm",
          confirm_label: "Close session",
          destructive: true,
        }}
        onSubmit={submit}
        onCancel={cancel}
      />,
    );
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: /^Cancel$/ }),
    );
    await userEvent.setup().keyboard("{Enter}");
    expect(cancel).toHaveBeenCalledOnce();
    expect(submit).not.toHaveBeenCalled();
  });

  it("does not select an option when Enter is pressed on the header cancel button", async () => {
    const submit = vi.fn();
    const cancel = vi.fn();
    render(
      <QuickInput
        title="Choice"
        mode={{ kind: "pick", choices: [{ id: "one", label: "One" }] }}
        onSubmit={submit}
        onCancel={cancel}
      />,
    );
    const user = userEvent.setup();
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(
      screen.getByLabelText("Cancel quick input"),
    );
    await user.keyboard("{Enter}");
    expect(cancel).toHaveBeenCalledOnce();
    expect(submit).not.toHaveBeenCalled();
  });

  it("keeps Escape available while waiting for an asynchronous operation", async () => {
    const cancel = vi.fn();
    render(
      <QuickInput
        title="Connecting"
        mode={{ kind: "progress" }}
        onSubmit={vi.fn()}
        onCancel={cancel}
      />,
    );
    expect(document.activeElement).toBe(screen.getByRole("dialog"));
    await userEvent.setup().keyboard("{Escape}");
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("supports keyboard choices, masks secrets, and cancels with Escape", async () => {
    const submit = vi.fn();
    const cancel = vi.fn();
    const { rerender } = render(
      <QuickInput
        title="Choice"
        mode={{
          kind: "pick",
          choices: [
            { id: "one", label: "One" },
            { id: "two", label: "Two" },
          ],
        }}
        onSubmit={submit}
        onCancel={cancel}
      />,
    );
    const user = userEvent.setup();
    await user.keyboard("{ArrowDown}{Enter}");
    expect(submit).toHaveBeenCalledWith("two");
    rerender(
      <QuickInput
        key="secret"
        title="Password"
        mode={{ kind: "input", label: "Password", secret: true }}
        onSubmit={submit}
        onCancel={cancel}
      />,
    );
    const password = screen.getByLabelText("Password", {
      selector: "input",
    }) as HTMLInputElement;
    expect(password.getAttribute("type")).toBe("password");
    await user.type(password, "temporary-secret{Enter}");
    expect(submit).toHaveBeenLastCalledWith("temporary-secret");
    expect(password.value).toBe("");
    await user.keyboard("{Escape}");
    expect(cancel).toHaveBeenCalledOnce();
  });
});
