import { describe, expect, it } from "vitest";
import { viewPanes } from "./viewLayout";
import type { ViewLayout } from "../../lib/types";

const leaf = (terminal_id: string): ViewLayout => ({ kind: "terminal", terminal_id });

describe("server view geometry", () => {
  it("composes nested splits without changing terminal identities", () => {
    expect(viewPanes({ kind: "split", axis: "horizontal", children: [leaf("a"), { kind: "split", axis: "vertical", children: [leaf("b"), leaf("c")] }] }, {})).toEqual([
      { terminal_id: "a", left: 0, top: 0, width: 50, height: 100, visible: true },
      { terminal_id: "b", left: 50, top: 0, width: 50, height: 50, visible: true },
      { terminal_id: "c", left: 50, top: 50, width: 50, height: 50, visible: true },
    ]);
  });
  it("keeps hidden tab terminals mounted and clamps selection after removal", () => {
    const layout: ViewLayout = { kind: "tabs", children: [leaf("a"), leaf("b")] };
    const panes = viewPanes(layout, { root: 5 });
    expect(panes.map((pane) => [pane.terminal_id, pane.visible])).toEqual([["a", false], ["b", true]]);
  });
});
