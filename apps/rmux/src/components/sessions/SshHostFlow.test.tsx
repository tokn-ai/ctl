// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { SshHostFlow } from "./SshHostFlow";
import {
  cancelSshProbe,
  forgetSshCredentials,
  probeSshHost,
  respondSshPrompt,
} from "../../lib/tauri";
import type { SshPrompt } from "../../lib/types";

vi.mock("../../lib/tauri", () => ({
  probeSshHost: vi.fn(),
  cancelSshProbe: vi.fn(async () => undefined),
  respondSshPrompt: vi.fn(async () => undefined),
  forgetSshCredentials: vi.fn(async () => undefined),
}));
afterEach(cleanup);
beforeEach(() => vi.clearAllMocks());

function setup() {
  const save = vi.fn(async () => undefined);
  const close = vi.fn();
  render(
    <StrictMode>
      <SshHostFlow
        suggestions={[]}
        warning={null}
        onSaveHost={save}
        onActivateHost={vi.fn(() => true)}
        onConnected={vi.fn()}
        onClose={close}
      />
    </StrictMode>,
  );
  return { save, close, user: userEvent.setup() };
}

async function details(user: ReturnType<typeof userEvent.setup>) {
  await user.type(
    screen.getByLabelText("SSH host"),
    "rmux@127.0.0.1:2222{Enter}",
  );
  await user.clear(screen.getByLabelText("Name / SSH alias"));
  await user.type(
    screen.getByLabelText("Name / SSH alias"),
    "rmux-test{Enter}",
  );
}

describe("SSH host quick-input flow", () => {
  it("forgets unsaved credentials when the storage step is cancelled", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(undefined);
    const { user, save, close } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await screen.findByRole("dialog", { name: "Save host" });
    await user.keyboard("{Escape}");
    expect(forgetSshCredentials).toHaveBeenCalledWith(
      expect.objectContaining({ destination: "rmux-test" }),
    );
    expect(save).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledOnce();
  });

  it("requires an explicit trust choice and ignores late prompts after cancellation", async () => {
    let prompt: ((value: SshPrompt) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementation(
      (_target, _attempt, callback) => {
        prompt = callback;
        return new Promise(() => undefined);
      },
    );
    const { user, close } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await act(async () =>
      prompt?.({
        prompt_id: "host-key",
        kind: "confirm",
        message: "Verify fingerprint SHA256:test",
      }),
    );
    expect(screen.getByRole("button", { name: /^Cancel$/ })).toBe(
      document.activeElement,
    );
    expect(respondSshPrompt).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Trust and connect" }));
    expect(respondSshPrompt).toHaveBeenCalledWith(
      expect.any(String),
      "host-key",
      "yes",
    );
    await user.keyboard("{Escape}");
    await act(async () =>
      prompt?.({
        prompt_id: "late",
        kind: "secret",
        message: "Late password:",
      }),
    );
    expect(screen.queryByText("Late password:")).toBeNull();
    expect(close).toHaveBeenCalledOnce();
  });

  it("keeps failed storage writes recoverable without connecting again", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(undefined);
    const { user, save, close } = setup();
    save.mockRejectedValueOnce(new Error("Alias already exists"));
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await user.click(
      await screen.findByRole("option", { name: /OpenSSH config/ }),
    );
    await screen.findByText("Alias already exists");
    expect(close).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /This app only/ }));
    expect(close).toHaveBeenCalledOnce();
    expect(probeSshHost).toHaveBeenCalledOnce();
  });

  it("types every stage, verifies before saving, and keeps the save location choice", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(undefined);
    const { save, user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await user.type(
      screen.getByRole("textbox", { name: "Identity file" }),
      "~/.ssh/local.id_rsa{Enter}",
    );
    await screen.findByText(
      "Connection verified. Where should this host be saved?",
    );
    expect(save).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /This app only/ }));
    expect(save).toHaveBeenCalledWith(
      {
        alias: "rmux-test",
        hostname: "127.0.0.1",
        user: "rmux",
        port: 2222,
        identity_file: "~/.ssh/local.id_rsa",
      },
      "local_storage",
    );
  });

  it("brokers masked SSH prompts and cancels the native attempt on Escape", async () => {
    let prompt: ((value: SshPrompt) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementation(
      (_target, _attempt, callback) => {
        prompt = callback;
        return new Promise(() => undefined);
      },
    );
    const { save, close, user } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /Password \/ interactive/ }),
    );
    await act(async () =>
      prompt?.({ prompt_id: "secret-1", kind: "secret", message: "Password:" }),
    );
    expect(screen.getByLabelText("SSH response").getAttribute("type")).toBe(
      "password",
    );
    await user.type(
      screen.getByLabelText("SSH response"),
      "temporary-secret{Enter}",
    );
    expect(respondSshPrompt).toHaveBeenCalledWith(
      expect.any(String),
      "secret-1",
      "temporary-secret",
    );
    await user.keyboard("{Escape}");
    expect(cancelSshProbe).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledOnce();
    expect(save).not.toHaveBeenCalled();
  });

  it("keeps preflight errors visible and lets the user backtrack", async () => {
    vi.mocked(probeSshHost).mockRejectedValue({
      message: "ctld: command not found",
    });
    const { user, save } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("ctld"),
    );
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(
      screen.getByRole("dialog", { name: "Authentication · 3/3" }),
    ).toBeTruthy();
    expect(save).not.toHaveBeenCalled();
  });
});
