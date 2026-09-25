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
      target: { kind: "ssh", destination: "rmux-docker" } as const,
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
