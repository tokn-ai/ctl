// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { QuickInput } from "./QuickInput";

afterEach(cleanup);

describe("QuickInput interactions", () => {
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
    expect(
      screen
        .getByLabelText("Password", { selector: "input" })
        .getAttribute("type"),
    ).toBe("password");
    await user.keyboard("{Escape}");
    expect(cancel).toHaveBeenCalledOnce();
  });
});
