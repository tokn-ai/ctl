import type { ViewLayout } from "../../lib/types";

export interface PaneRect {
  terminal_id: string;
  left: number;
  top: number;
  width: number;
  height: number;
  visible: boolean;
}

// Derive decorative separators from the server's rectangles, including nested
// splits. This never changes terminal geometry.
export function viewDividers(layout: ViewLayout, panes: readonly PaneRect[]) {
  const dividers: { path: string; vertical: boolean; left: number; top: number; length: number }[] = [];
  function visit(node: ViewLayout, path: string): Omit<PaneRect, "terminal_id" | "visible"> | undefined {
    if (node.kind === "terminal") return panes.find((pane) => pane.terminal_id === node.terminal_id && pane.visible);
    const children = node.children.map((child, index) => visit(child, `${path}.${index}`)).filter((rect) => rect !== undefined);
    if (!children.length) return;
    const left = Math.min(...children.map((rect) => rect.left));
    const top = Math.min(...children.map((rect) => rect.top));
    const width = Math.max(...children.map((rect) => rect.left + rect.width)) - left;
    const height = Math.max(...children.map((rect) => rect.top + rect.height)) - top;
    if (node.kind === "split") {
      const vertical = node.axis === "horizontal";
      children.slice(1).forEach((next, index) => {
        const previous = children[index];
        dividers.push({
          path: `${path}.${index}`, vertical,
          left: vertical ? (previous.left + previous.width + next.left) / 2 : left,
          top: vertical ? top : (previous.top + previous.height + next.top) / 2,
          length: vertical ? height : width,
        });
      });
    }
    return { left, top, width, height };
  }
  visit(layout, "root");
  return dividers;
}

export function adjacentPane(panes: readonly PaneRect[], terminal_id: string, direction: string): string | undefined {
  const origin = panes.find((pane) => pane.terminal_id === terminal_id);
  if (!origin) return;
  const horizontal = direction === "left" || direction === "right";
  const forward = direction === "right" || direction === "down" ? 1 : -1;
  const x = origin.left + origin.width / 2;
  const y = origin.top + origin.height / 2;
  return panes.filter((pane) => pane.visible && pane.terminal_id !== terminal_id)
    .map((pane) => {
      const dx = pane.left + pane.width / 2 - x;
      const dy = pane.top + pane.height / 2 - y;
      return { id: pane.terminal_id, distance: (horizontal ? dx : dy) * forward, cross: Math.abs(horizontal ? dy : dx) };
    }).filter((candidate) => candidate.distance > 0.01)
    .sort((a, b) => a.cross - b.cross || a.distance - b.distance)[0]?.id;
}

export function swapPanes(layout: ViewLayout, first: string, second: string): ViewLayout {
  if (layout.kind === "terminal") return { ...layout, terminal_id: layout.terminal_id === first ? second : layout.terminal_id === second ? first : layout.terminal_id };
  return { ...layout, children: layout.children.map((child) => swapPanes(child, first, second)) };
}
