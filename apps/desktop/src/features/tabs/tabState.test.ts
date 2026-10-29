import { describe, expect, it } from "vitest";
import type { SessionSummary } from "../../lib/types";
import { sessionKey } from "../targets/targets";
import {
  closeTerminalTab,
  openTerminalTab,
  reconcileTerminalTabs,
  syncTabTerminalSize,
} from "./tabState";

function session(id: string): SessionSummary {
  return {
    target: { kind: "local" },
    session_id: id,
    name: id,
    status: "running",
    terminal_size: {
      columns: 80,
      rows: 24,
      pixel_width: null,
      pixel_height: null,
    },
    next_sequence: "0",
  };
}

describe("terminal tab state", () => {
  it("inserts a new session after its source without mutating the existing tabs", () => {
    const tabs = [session("first"), session("second"), session("third")];
    const created = session("created");

    expect(openTerminalTab(tabs, created, sessionKey(tabs[1]))).toEqual([
      tabs[0], tabs[1], created, tabs[2],
    ]);
    expect(tabs.map((tab) => tab.session_id)).toEqual(["first", "second", "third"]);
  });

  it("appends when the source tab was closed or no insertion anchor was supplied", () => {
    const tabs = [session("first"), session("second")];
    const created = session("created");

    expect(openTerminalTab(tabs, created, sessionKey(session("closed")))).toEqual([...tabs, created]);
    expect(openTerminalTab(tabs, created)).toEqual([...tabs, created]);
  });

  it("updates an already-open session without moving it to the supplied anchor", () => {
    const tabs = [session("first"), session("second"), session("third")];
    const updated = { ...tabs[0], name: "renamed" };

    expect(openTerminalTab(tabs, updated, sessionKey(tabs[1]))).toEqual([
      updated, tabs[1], tabs[2],
    ]);
    expect(tabs[0].name).toBe("first");
  });

  it("opens a session once and preserves tab order", () => {
    const first = session("first");
    const second = session("second");
    const tabs = openTerminalTab(openTerminalTab([], first), second);

    expect(openTerminalTab(tabs, first).map((tab) => tab.session_id)).toEqual([
      "first",
      "second",
    ]);
  });

  it("opens equal daemon session ids from different targets as separate tabs", () => {
    const local = session("same");
    const remote = {
      ...session("same"),
      target: { kind: "ssh", destination: "ctmux-docker" } as const,
    };

    expect(openTerminalTab(openTerminalTab([], local), remote)).toEqual([
      local,
      remote,
    ]);
  });

  it("selects the right neighbor, then the left neighbor, after close", () => {
    const tabs = [session("first"), session("second"), session("third")];

    expect(closeTerminalTab(tabs, sessionKey(tabs[1])).nextTab?.session_id).toBe(
      "third",
    );
    expect(closeTerminalTab(tabs, sessionKey(tabs[2])).nextTab?.session_id).toBe(
      "second",
    );
  });

  it("keeps only listed tabs except the active transition", () => {
    const tabs = [session("first"), session("second")];
    const listed = [{ ...session("second"), name: "updated" }];

    expect(reconcileTerminalTabs(tabs, listed, sessionKey(tabs[0]))).toEqual([
      tabs[0],
      listed[0],
    ]);
    expect(reconcileTerminalTabs(tabs, listed, null)).toEqual([listed[0]]);
  });

  it("keeps tab geometry synchronized with the active attachment", () => {
    const tabs = [session("first"), session("second")];
    const terminalSize = {
      columns: 107,
      rows: 34,
      pixel_width: null,
      pixel_height: null,
    };

    expect(syncTabTerminalSize(tabs, sessionKey(tabs[1]), terminalSize)).toEqual([
      tabs[0],
      { ...tabs[1], terminal_size: terminalSize },
    ]);
  });
});
