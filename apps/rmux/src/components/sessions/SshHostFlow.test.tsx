// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { SshHostFlow } from "./SshHostFlow";
import {
  cancelSshProbe,
  forgetSshCredentials,
  listSshIdentityFiles,
  probeSshHost,
  respondSshPrompt,
} from "../../lib/tauri";
import type { SshPrompt } from "../../lib/types";

vi.mock("../../lib/tauri", () => ({
  probeSshHost: vi.fn(),
  cancelSshProbe: vi.fn(async () => undefined),
  respondSshPrompt: vi.fn(async () => undefined),
  forgetSshCredentials: vi.fn(async () => undefined),
  listSshIdentityFiles: vi.fn(),
}));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(listSshIdentityFiles).mockResolvedValue({
    identity_files: [],
    warnings: [],
  });
});

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
  it("discovers identities only on the identity step and connects with a selected path", async () => {
    vi.mocked(listSshIdentityFiles).mockResolvedValue({
      identity_files: [
        {
          path: "/test-home/.ssh/local.id_rsa",
          display_path: "~/.ssh/local.id_rsa",
        },
      ],
      warnings: [],
    });
    vi.mocked(probeSshHost).mockResolvedValue(undefined);
    const { user } = setup();
    await details(user);
    expect(listSshIdentityFiles).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByRole("option", { name: "~/.ssh/local.id_rsa" });
    await user.keyboard("{ArrowDown}{Enter}");
    expect(probeSshHost).toHaveBeenCalledWith(
      expect.objectContaining({
        identity_file: "/test-home/.ssh/local.id_rsa",
      }),
      expect.any(String),
      expect.any(Function),
    );
  });

  it("allows a manual identity when discovery fails", async () => {
    vi.mocked(listSshIdentityFiles).mockRejectedValue(
      new Error("Permission denied"),
    );
    vi.mocked(probeSshHost).mockResolvedValue(undefined);
    const { user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByText(/Could not list ~\/.ssh: Permission denied/);
    await user.type(
      screen.getByRole("combobox", { name: "Identity file" }),
      "/custom/key{Enter}",
    );
    expect(probeSshHost).toHaveBeenCalledWith(
      expect.objectContaining({ identity_file: "/custom/key" }),
      expect.any(String),
      expect.any(Function),
    );
  });

  it("ignores a stale discovery after leaving and reopening the identity step", async () => {
    let resolveFirst:
      | ((catalog: { identity_files: []; warnings: string[] }) => void)
      | undefined;
    vi.mocked(listSshIdentityFiles).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveFirst = resolve;
        }),
    );
    const { user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByText("Loading identity files…");
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByText(
      "No identity-file candidates in ~/.ssh. Enter a path manually.",
    );
    await act(async () =>
      resolveFirst?.({ identity_files: [], warnings: ["stale error"] }),
    );
    expect(screen.queryByText("stale error")).toBeNull();
    expect(listSshIdentityFiles).toHaveBeenCalledTimes(2);
  });

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
      screen.getByRole("combobox", { name: "Identity file" }),
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
      message: "ctl-agent: command not found",
    });
    const { user, save } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("ctl-agent"),
    );
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(
      screen.getByRole("dialog", { name: "Authentication · 3/3" }),
    ).toBeTruthy();
    expect(save).not.toHaveBeenCalled();
  });
});
