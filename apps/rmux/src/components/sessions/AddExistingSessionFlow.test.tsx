// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ConnectionTarget,
  SessionListResponse,
  SessionSummary,
} from "../../lib/types";
import { AddExistingSessionFlow } from "./AddExistingSessionFlow";
import { probeSshHost } from "../../lib/tauri";

const list = vi.hoisted(() => vi.fn());
vi.mock("../../lib/tauri", async (original) => ({
  ...(await original<object>()),
  listSessions: list,
  probeSshHost: vi.fn(),
  cancelSshProbe: vi.fn().mockResolvedValue(undefined),
}));
const verify = vi.fn(async (target: ConnectionTarget) => target);
const targets: ConnectionTarget[] = [
  { kind: "local" },
  { kind: "ssh", destination: "remote", host_id: "remote-id" },
];
function session(id: string): SessionSummary {
  return {
    target: targets[1],
    session_id: id,
    name: id,
    status: "running",
    next_sequence: "0",
    terminal_size: {
      columns: 80,
      rows: 24,
      pixel_width: null,
      pixel_height: null,
    },
  };
}
beforeEach(() => {
  list.mockReset();
  verify.mockReset().mockImplementation(async (target: ConnectionTarget) => target);
  vi.mocked(probeSshHost).mockReset().mockResolvedValue({ remote_id: "remote-environment", agent_version: "0.1.0" });
});
afterEach(cleanup);

describe("explicit discovery", () => {
  it("contacts only the selected host and does not import until a session is selected", async () => {
    list.mockResolvedValue({
      sessions: [session("known"), session("other-app")],
      shell_states: {},
    });
    const onAdd = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    const view = render(
      <AddExistingSessionFlow
        targets={targets}
        onVerifyHost={verify}
        known={[session("known")]}
        onAdd={onAdd}
        onClose={onClose}
      />,
    );
    expect(list).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("option", { name: "remote" }));
    await screen.findByRole("option", { name: /other-app/ });
    expect(list).toHaveBeenCalledExactlyOnceWith(targets[1]);
    expect(screen.queryByRole("option", { name: "known" })).toBeNull();
    expect(onAdd).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("option", { name: /other-app/ }));
    await waitFor(() =>
      expect(onAdd).toHaveBeenCalledExactlyOnceWith(session("other-app"), null),
    );
    view.rerender(
      <AddExistingSessionFlow
        targets={targets}
        onVerifyHost={verify}
        known={[session("known"), session("other-app")]}
        onAdd={onAdd}
        onClose={onClose}
      />,
    );
    await screen.findByText(/No additional running sessions/);
    fireEvent.click(screen.getByRole("option", { name: "Done" }));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("ignores late discovery results after leaving a host", async () => {
    let resolve!: (response: SessionListResponse) => void;
    list.mockImplementation(
      () =>
        new Promise((complete) => {
          resolve = complete;
        }),
    );
    const onAdd = vi.fn();
    render(
      <AddExistingSessionFlow
        targets={targets}
        onVerifyHost={verify}
        known={[]}
        onAdd={onAdd}
        onClose={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("option", { name: "remote" }));
    await waitFor(() => expect(list).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: "Previous step" }));
    await act(async () =>
      resolve({ sessions: [session("late")], shell_states: {} }),
    );
    expect(screen.queryByRole("option", { name: /late/ })).toBeNull();
    expect(onAdd).not.toHaveBeenCalled();
    expect(list).toHaveBeenCalledTimes(1);
  });

  it("shows actionable authentication failures and empty results", async () => {
    list
      .mockRejectedValueOnce({ message: "Permission denied" })
      .mockResolvedValue({ sessions: [], shell_states: {} });
    render(
      <AddExistingSessionFlow
        targets={targets}
        onVerifyHost={verify}
        known={[]}
        onAdd={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("option", { name: "remote" }));
    await screen.findByText("Permission denied");
    expect(screen.getByText(/Retry to reconnect/)).toBeTruthy();
    fireEvent.click(screen.getByRole("option", { name: "Retry discovery" }));
    await screen.findByText(/No additional running sessions/);
    expect(probeSshHost).toHaveBeenCalledTimes(2);
  });

  it("discovers local sessions without starting SSH authentication", async () => {
    list.mockResolvedValue({ sessions: [], shell_states: {} });
    render(<AddExistingSessionFlow targets={targets} known={[]} onVerifyHost={verify} onAdd={vi.fn()} onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("option", { name: "local" }));
    await screen.findByText(/No additional running sessions/);
    expect(list).toHaveBeenCalledExactlyOnceWith(targets[0]);
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("does not enumerate an endpoint that fails identity verification", async () => {
    verify.mockRejectedValue(new Error("The remote environment changed."));
    render(<AddExistingSessionFlow targets={targets} known={[]} onVerifyHost={verify} onAdd={vi.fn()} onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("option", { name: "remote" }));
    await screen.findByText("The remote environment changed.");
    expect(list).not.toHaveBeenCalled();
  });
});
