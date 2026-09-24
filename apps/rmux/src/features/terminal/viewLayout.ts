import type { ViewLayout } from "../../lib/types";

export interface PaneRect {
  terminal_id: string;
  left: number;
  top: number;
  width: number;
  height: number;
  visible: boolean;
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
