// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StrictMode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionTarget } from "../../lib/types";
import { NewShellFlow } from "./NewShellFlow";

const local: ConnectionTarget = { kind: "local" };
const remote: ConnectionTarget = {
  kind: "ssh",
  host_id: "remote-id",
  destination: "remote",
  hostname: "127.0.0.1",
  port: 2222,
  user: "rmux",
  identity_file: "/keys/test",
};

afterEach(cleanup);

function setup() {
  const create = vi
    .fn<
      (
        target: ConnectionTarget,
        working_directory: string | null,
      ) => Promise<void>
    >()
    .mockResolvedValue(undefined);
  const close = vi.fn();
  const props = { targets: [remote, local], onCreate: create, onClose: close };
  const view = render(
    <StrictMode>
      <NewShellFlow {...props} />
    </StrictMode>,
  );
  return { create, close, props, view, user: userEvent.setup() };
}

describe("new-shell quick-input flow", () => {
  it("defaults to Local and accepts a blank home directory with the keyboard", async () => {
    const { create, close, user } = setup();
    expect(document.activeElement).toBe(
      screen.getByRole("option", { name: "Local" }),
    );
    expect(create).not.toHaveBeenCalled();
    await user.keyboard("{Enter}");
    expect(document.activeElement).toBe(
      screen.getByLabelText("Working directory"),
    );
    expect(create).not.toHaveBeenCalled();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(close).toHaveBeenCalledOnce());
    expect(create).toHaveBeenCalledExactlyOnceWith(local, null);
  });

  it("creates on only the selected host and preserves a directory draft through Back", async () => {
    const { create, close, user } = setup();
    await user.keyboard("{ArrowDown}{Enter}");
    await user.type(
      screen.getByLabelText("Working directory"),
      "  /work/my project  ",
    );
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(create).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "remote" }));
    expect(
      (screen.getByLabelText("Working directory") as HTMLInputElement).value,
    ).toBe("  /work/my project  ");
    await user.click(screen.getByRole("button", { name: "Create shell" }));
    await waitFor(() => expect(close).toHaveBeenCalledOnce());
    expect(create).toHaveBeenCalledExactlyOnceWith(remote, "/work/my project");
  });

  it.each(["host", "directory"])(
    "cancels the %s step without creating a session",
    async (step) => {
      const { create, close, user } = setup();
      if (step === "directory") await user.keyboard("{Enter}");
      await user.keyboard("{Escape}");
      expect(close).toHaveBeenCalledOnce();
      expect(create).not.toHaveBeenCalled();
    },
  );

  it("shows creation errors inline and retains inputs for an explicit retry", async () => {
    const { create, close, user } = setup();
    create.mockRejectedValueOnce({ message: "Directory does not exist" });
    await user.click(screen.getByRole("option", { name: "remote" }));
    await user.type(
      screen.getByLabelText("Working directory"),
      "/missing{Enter}",
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Directory does not exist",
    );
    expect(close).not.toHaveBeenCalled();
    expect(
      (screen.getByLabelText("Working directory") as HTMLInputElement).value,
    ).toBe("/missing");
    await user.clear(screen.getByLabelText("Working directory"));
    await user.type(
      screen.getByLabelText("Working directory"),
      "/exists{Enter}",
    );
    await waitFor(() => expect(close).toHaveBeenCalledOnce());
    expect(create).toHaveBeenCalledTimes(2);
    expect(create).toHaveBeenLastCalledWith(remote, "/exists");
  });

  it("blocks repeated submission, Escape, and backdrop dismissal after creation starts", async () => {
    const { create, close, user } = setup();
    let resolve!: () => void;
    create.mockImplementationOnce(
      () =>
        new Promise<void>((done) => {
          resolve = done;
        }),
    );
    await user.keyboard("{Enter}");
    const form = screen.getByLabelText("Working directory").closest("form")!;
    act(() => {
      fireEvent.submit(form);
      fireEvent.submit(form);
    });
    expect(screen.getByRole("status").textContent).toContain(
      "Creating and opening",
    );
    expect(
      (screen.getByLabelText("Cancel quick input") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    await user.keyboard("{Enter}{Escape}");
    fireEvent.mouseDown(screen.getByRole("dialog").parentElement!);
    expect(create).toHaveBeenCalledOnce();
    expect(close).not.toHaveBeenCalled();
    await act(async () => resolve());
    expect(close).toHaveBeenCalledOnce();
  });

  it("ignores a late completion after unmount without pretending to cancel creation", async () => {
    const { create, close, view, user } = setup();
    let resolve!: () => void;
    create.mockImplementationOnce(
      () =>
        new Promise<void>((done) => {
          resolve = done;
        }),
    );
    await user.keyboard("{Enter}{Enter}");
    view.unmount();
    await act(async () => resolve());
    expect(create).toHaveBeenCalledOnce();
    expect(close).not.toHaveBeenCalled();
  });

  it("requires another explicit host choice if the selected host disappears", async () => {
    const { create, props, view, user } = setup();
    await user.click(screen.getByRole("option", { name: "remote" }));
    view.rerender(
      <StrictMode>
        <NewShellFlow {...props} targets={[local]} />
      </StrictMode>,
    );
    expect(screen.getByRole("alert").textContent).toContain(
      "no longer available",
    );
    expect(screen.queryByLabelText("Working directory")).toBeNull();
    expect(create).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "Local" }));
    await user.keyboard("{Enter}");
    expect(create).toHaveBeenCalledExactlyOnceWith(local, null);
  });
});
