import { describe, expect, it } from "vitest";
import { adjacentPane, layoutTopology, swapPanes, viewDividers } from "./viewLayout";
import type { ViewLayout } from "../../lib/types";

const leaf = (terminal_id: string): ViewLayout => ({ kind: "terminal", terminal_id });

describe("server view geometry", () => {
  const layout: ViewLayout = {
    kind: "split", axis: "horizontal", weights: [3, 2],
    children: [leaf("a"), { kind: "split", axis: "vertical", weights: [2, 1], children: [leaf("b"), leaf("c")] }],
  };
  const panes = [
    { terminal_id: "a", left: 0, top: 0, width: 50, height: 40, visible: true },
    { terminal_id: "b", left: 51, top: 0, width: 49, height: 20, visible: true },
    { terminal_id: "c", left: 51, top: 21, width: 49, height: 19, visible: true },
  ];

  it("centers dividers in server-allocated gaps for nested splits", () => {
    expect(viewDividers(layout, panes)).toEqual([
      { path: "root.1.0", split_path: [1], boundary: 0, vertical: false, left: 51, top: 20.5, length: 49 },
      { path: "root.0", split_path: [], boundary: 0, vertical: true, left: 50.5, top: 0, length: 40 },
    ]);
  });

  it("navigates and swaps panes across nested splits", () => {
    expect(adjacentPane(panes, "b", "down")).toBe("c");
    expect(adjacentPane(panes, "c", "left")).toBe("a");
    expect(swapPanes(layout, "a", "c")).toEqual({
      kind: "split", axis: "horizontal", weights: [3, 2],
      children: [leaf("c"), { kind: "split", axis: "vertical", weights: [2, 1], children: [leaf("b"), leaf("a")] }],
    });
  });

  it("keeps split identity stable across weight changes and distinguishes topology changes", () => {
    expect(layoutTopology({ ...layout, weights: [1, 4] })).toBe(layoutTopology(layout));
    expect(layoutTopology(swapPanes(layout, "a", "c"))).not.toBe(layoutTopology(layout));
    expect(viewDividers(layout, panes.filter((pane) => pane.terminal_id !== "b"))).toEqual([
      { path: "root.0", split_path: [], boundary: 0, vertical: true, left: 50.5, top: 0, length: 40 },
    ]);
  });
});
