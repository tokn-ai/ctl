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
// splits and the selected tab. This never changes terminal geometry.
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

// Flatten layout geometry so changing the tree never remounts terminal renderers.
export function viewPanes(layout: ViewLayout, selected_tabs: Readonly<Record<string, number>>): PaneRect[] {
  const panes: PaneRect[] = [];
  function visit(node: ViewLayout, path: string, rect: Omit<PaneRect, "terminal_id">) {
    if (node.kind === "terminal") {
      panes.push({ ...rect, terminal_id: node.terminal_id });
      return;
    }
    node.children.forEach((child, index) => {
      const next = { ...rect };
      if (node.kind === "tabs") {
        const selected = Math.min(selected_tabs[path] ?? 0, node.children.length - 1);
        next.visible = rect.visible && index === selected;
      } else if (node.axis === "horizontal") {
        next.width /= node.children.length;
        next.left += index * next.width;
      } else {
        next.height /= node.children.length;
        next.top += index * next.height;
      }
      visit(child, `${path}.${index}`, next);
    });
  }
  visit(layout, "root", { left: 0, top: 0, width: 100, height: 100, visible: true });
  return panes;
}

export function viewTabs(layout: ViewLayout): { path: string; count: number }[] {
  const groups: { path: string; count: number }[] = [];
  function visit(node: ViewLayout, path: string) {
    if (node.kind === "terminal") return;
    if (node.kind === "tabs") groups.push({ path, count: node.children.length });
    node.children.forEach((child, index) => visit(child, `${path}.${index}`));
  }
  visit(layout, "root");
  return groups;
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
