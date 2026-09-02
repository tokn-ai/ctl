// @vitest-environment jsdom
import { StrictMode } from "react";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionTarget, SessionSummary } from "../../lib/types";
import { sessionKey } from "../targets/targets";
import { useWorkspaceConnections } from "./useWorkspaceConnections";

const local: ConnectionTarget = { kind: "local" };
const remote: ConnectionTarget = {
  kind: "ssh",
  host_id: "remote",
  destination: "remote",
};
const other: ConnectionTarget = {
  kind: "ssh",
  host_id: "other",
  destination: "other",
};

function session(target: ConnectionTarget, session_id: string): SessionSummary {
  return {
    target,
    session_id,
    name: session_id,
    status: "unknown",
    next_sequence: "0",
    terminal_size: {
      columns: 80,
      rows: 24,
      pixel_width: null,
      pixel_height: null,
    },
  };
}

function setup(tabs: SessionSummary[], active = tabs[0]) {
  const initial: Parameters<typeof useWorkspaceConnections>[0] = {
    ready: true,
    closing: false,
    tabs,
    active_tab_key: active ? sessionKey(active) : null,
    activateTab: vi.fn(async () => undefined),
    refreshHost: vi.fn(async () => undefined),
  };
  const hook = renderHook(useWorkspaceConnections, {
    initialProps: initial,
    wrapper: StrictMode,
  });
  return { ...hook, initial };
}

function pending() {
  let resolve!: () => void;
  const promise = new Promise<void>((complete) => {
    resolve = complete;
  });
  return { resolve, promise };
}

afterEach(cleanup);

describe("workspace connection policy", () => {
  it("waits for hydration and attempts local restoration only once", () => {
    const tab = session(local, "local");
    const initial = {
      ready: false,
      closing: false,
      tabs: [tab],
      active_tab_key: sessionKey(tab),
      activateTab: vi.fn(async () => undefined),
      refreshHost: vi.fn(async () => undefined),
    };
    const { rerender } = renderHook(useWorkspaceConnections, {
      initialProps: initial,
      wrapper: StrictMode,
    });
    expect(initial.activateTab).not.toHaveBeenCalled();
    rerender({ ...initial, ready: true });
    expect(initial.activateTab).toHaveBeenCalledExactlyOnceWith(tab);
    rerender({
      ...initial,
      ready: true,
      tabs: [{ ...tab, status: "missing" }],
    });
    rerender({ ...initial, ready: true, tabs: [] });
    expect(initial.activateTab).toHaveBeenCalledOnce();
    expect(initial.refreshHost).not.toHaveBeenCalled();
  });

  it("does not select a background local tab when the restored selection is remote", () => {
    const { initial } = setup([
      session(remote, "active"),
      session(local, "local"),
    ]);
    expect(initial.activateTab).not.toHaveBeenCalled();
    expect(initial.refreshHost).not.toHaveBeenCalled();
  });

  it("does not create or attach anything in an empty workspace", () => {
    const { initial } = setup([]);
    expect(initial.activateTab).not.toHaveBeenCalled();
    expect(initial.refreshHost).not.toHaveBeenCalled();
  });

  it("prefers the selected tab on the connected host after inspecting that host", async () => {
    const first = session(remote, "first");
    const selected = session(remote, "selected");
    const { result, initial } = setup([first, selected], selected);
    vi.mocked(initial.refreshHost).mockImplementationOnce(async (target) => {
      expect(target).toEqual(remote);
      expect(initial.activateTab).not.toHaveBeenCalled();
    });
    await act(async () => result.current(remote));
    expect(initial.activateTab).toHaveBeenCalledExactlyOnceWith(selected);
    expect(initial.refreshHost).toHaveBeenCalledExactlyOnceWith(remote);
  });

  it("resumes the first open tab of the connected host when another host was selected", async () => {
    const active = session(other, "same-id");
    const first = session(remote, "same-id");
    const { result, initial } = setup([
      active,
      first,
      session(remote, "second"),
    ]);
    await act(async () => result.current(remote));
    expect(initial.activateTab).toHaveBeenCalledExactlyOnceWith(first);
  });

  it("does not reopen detached sessions or switch to a different host's tab", async () => {
    const { result, initial } = setup([session(other, "other")]);
    await act(async () => result.current(remote));
    expect(initial.refreshHost).toHaveBeenCalledExactlyOnceWith(remote);
    expect(initial.activateTab).not.toHaveBeenCalled();
  });

  it.each(["selection", "closed_tab", "closing_window", "unmount"])(
    "ignores late inspection after %s changes the connection intent",
    async (change) => {
      const selected = session(remote, "selected");
      const otherTab = session(other, "other");
      const { result, initial, rerender, unmount } = setup([
        selected,
        otherTab,
      ]);
      const inspection = pending();
      vi.mocked(initial.refreshHost).mockReturnValueOnce(inspection.promise);
      let connection!: Promise<void>;
      act(() => {
        connection = result.current(remote);
      });
      if (change === "unmount") unmount();
      else
        rerender({
          ...initial,
          active_tab_key:
            change === "selection"
              ? sessionKey(otherTab)
              : initial.active_tab_key,
          tabs: change === "closed_tab" ? [otherTab] : initial.tabs,
          closing: change === "closing_window",
        });
      await act(async () => {
        inspection.resolve();
        await connection;
      });
      expect(initial.activateTab).not.toHaveBeenCalled();
    },
  );

  it("uses updated tab metadata after inspection", async () => {
    const selected = session(remote, "selected");
    const { result, initial, rerender } = setup([selected]);
    const inspection = pending();
    vi.mocked(initial.refreshHost).mockReturnValueOnce(inspection.promise);
    let connection!: Promise<void>;
    act(() => {
      connection = result.current(remote);
    });
    const updated: SessionSummary = {
      ...selected,
      status: "running",
      next_sequence: "15",
    };
    rerender({ ...initial, tabs: [updated] });
    await act(async () => {
      inspection.resolve();
      await connection;
    });
    expect(initial.activateTab).toHaveBeenCalledExactlyOnceWith(updated);
  });

  it("does not resume an older host connection after a newer one completes", async () => {
    const selected = session(remote, "selected");
    const otherTab = session(other, "other");
    const { result, initial } = setup([selected, otherTab]);
    const inspection = pending();
    vi.mocked(initial.refreshHost).mockReturnValueOnce(inspection.promise);
    let older!: Promise<void>;
    act(() => {
      older = result.current(remote);
    });
    await act(async () => result.current(other));
    await act(async () => {
      inspection.resolve();
      await older;
    });
    expect(initial.activateTab).toHaveBeenCalledExactlyOnceWith(otherTab);
  });
});
