import type { ReactNode } from "react";
import type { ShellType, ViewLayout } from "../../lib/types";

interface Props {
  shell_type?: ShellType;
  layout?: ViewLayout | null;
  size?: number;
  class_name?: string;
}

const shell_names: Record<ShellType, string> = {
  bash: "Bash",
  zsh: "Zsh",
  fish: "Fish",
  pwsh: "PowerShell",
  cmd: "Command Prompt",
  sh: "POSIX shell",
  unknown: "Terminal",
};

const dollar = <path d="M14.5 8.5h-4a2 2 0 0 0 0 4h3a2 2 0 0 1 0 4h-4M12 7v11" />;
const shell_glyphs: Record<ShellType, ReactNode> = {
  bash: <><path d="m12 2.5 9 5v9l-9 5-9-5v-9z" />{dollar}</>,
  zsh: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="m9 16 6-8" /><circle cx="9" cy="9" r="1" /><circle cx="15" cy="15" r="1" /></>,
  fish: <><path d="M16 12c-3.5-6-9-6-13 0 4 6 9.5 6 13 0Zm0 0 5-5v10z" /><circle cx="7" cy="11" r=".65" fill="currentColor" stroke="none" /></>,
  pwsh: <><path d="M6 4h16l-4 16H2zM9 8l4 4-6 4m6 0h4" /></>,
  cmd: <><rect x="3" y="4" width="18" height="16" rx="1" /><path d="M3 7h18m-11 3 3 3-3 3m6 0h2" /><path d="M7 11H6v4h1" /></>,
  sh: <><rect x="3" y="4" width="18" height="16" rx="2" />{dollar}</>,
  unknown: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="m7 9 3 3-3 3m6 0h4" /></>,
};

function paneCount(layout: ViewLayout): number {
  return layout.kind === "terminal"
    ? 1
    : layout.children.reduce((total, child) => total + paneCount(child), 0);
}

export function sessionIconLabel(shell_type: ShellType = "unknown", layout?: ViewLayout | null): string {
  const pane_count = layout ? paneCount(layout) : 0;
  return pane_count > 1 ? `${pane_count}-pane session layout` : `${shell_names[shell_type]} session`;
}

interface Region {
  x: number;
  y: number;
  width: number;
  height: number;
}

function visibleLengths(weights: number[], span: number): number[] {
  // Preserve proportions unless a divider would disappear into its neighbour.
  const minimum = Math.min(2.5, span / weights.length);
  const largest_weight = Math.max(...weights);
  const scaled = weights.map((weight) => weight / largest_weight);
  const lengths = weights.map(() => 0);
  let available = span;
  let pending = weights.map((_, index) => index);

  while (pending.length > 0) {
    const total = pending.reduce((sum, index) => sum + scaled[index], 0);
    const small = pending.filter((index) => available * scaled[index] / total < minimum);
    if (small.length === 0) {
      pending.forEach((index) => { lengths[index] = available * scaled[index] / total; });
      break;
    }
    small.forEach((index) => { lengths[index] = minimum; });
    available -= minimum * small.length;
    pending = pending.filter((index) => !small.includes(index));
  }

  return lengths;
}

/** Subdivide the full layout, independent of the currently zoomed terminal. */
function layoutDividers(layout: ViewLayout, region: Region, paths: string[]): void {
  if (layout.kind === "terminal") return;

  const valid_weights = layout.weights?.length === layout.children.length
    && layout.weights.every((weight) => Number.isFinite(weight) && weight > 0);
  const children = layout.children
    .map((child, index) => ({ child, weight: valid_weights ? layout.weights![index] : 1 }))
    .filter(({ child }) => paneCount(child) > 0);
  if (children.length === 0) return;

  const horizontal = layout.axis === "horizontal";
  const span = horizontal ? region.width : region.height;
  const lengths = visibleLengths(children.map(({ weight }) => weight), span);
  let offset = 0;

  children.forEach(({ child }, index) => {
    const length = index === children.length - 1
      ? span - offset
      : lengths[index];
    const child_region = horizontal
      ? { ...region, x: region.x + offset, width: length }
      : { ...region, y: region.y + offset, height: length };
    layoutDividers(child, child_region, paths);
    offset += length;
    if (index === children.length - 1) return;

    paths.push(horizontal
      ? `M${region.x + offset} ${region.y}v${region.height}`
      : `M${region.x} ${region.y + offset}h${region.width}`);
  });
}

/** The shell for one terminal, or a miniature of the session's split layout. */
export function SessionIcon({ shell_type = "unknown", layout, size = 16, class_name }: Props) {
  const pane_count = layout ? paneCount(layout) : 0;
  const is_split = pane_count > 1;
  const label = sessionIconLabel(shell_type, layout);
  const dividers: string[] = [];
  if (layout && is_split) {
    layoutDividers(layout, { x: 3, y: 4, width: 18, height: 16 }, dividers);
  }

  return (
    <svg
      className={class_name}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={is_split ? "1.4" : "1.6"}
      strokeLinecap="round"
      strokeLinejoin="round"
      role="img"
      aria-label={label}
      focusable="false"
    >
      <title>{label}</title>
      {is_split ? <>
        <rect x="3" y="4" width="18" height="16" rx="1" />
        <path d={dividers.join(" ")} />
      </> : shell_glyphs[shell_type]}
    </svg>
  );
}
