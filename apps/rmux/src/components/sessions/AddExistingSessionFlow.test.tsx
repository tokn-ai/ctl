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

const list = vi.hoisted(() => vi.fn());
vi.mock("../../lib/tauri", () => ({ listSessions: list }));
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
        known={[]}
        onAdd={onAdd}
        onClose={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("option", { name: "remote" }));
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
        known={[]}
        onAdd={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("option", { name: "remote" }));
    await screen.findByText("Permission denied");
    expect(screen.getByText(/Connect host first/)).toBeTruthy();
    fireEvent.click(screen.getByRole("option", { name: "Retry discovery" }));
    await screen.findByText(/No additional running sessions/);
  });
});
