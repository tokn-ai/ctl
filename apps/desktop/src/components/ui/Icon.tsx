import type { ReactNode } from "react";

const paths = {
  terminal: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="m7 9 3 3-3 3m6 0h4" /></>,
  tasks: <><rect x="5" y="3" width="14" height="18" rx="2" /><path d="m8 8 1 1 2-2m2 1h3m-8 6 1 1 2-2m2 1h3" /></>,
  ports: <><circle cx="6" cy="6" r="3" /><circle cx="18" cy="18" r="3" /><path d="M9 6h6a3 3 0 0 1 3 3v6M6 9v12" /></>,
  server: <><rect x="3" y="3" width="18" height="7" rx="1" /><rect x="3" y="14" width="18" height="7" rx="1" /><path d="M7 6.5h.01M7 17.5h.01M11 6.5h6m-6 11h6" /></>,
  monitor: <><rect x="3" y="3" width="18" height="14" rx="1" /><path d="M8 21h8m-4-4v4" /></>,
  chevron_right: <path d="m9 5 7 7-7 7" />,
  chevron_down: <path d="m5 9 7 7 7-7" />,
  plus: <path d="M12 5v14M5 12h14" />,
  close: <path d="m6 6 12 12M6 18 18 6" />,
  refresh: <><path d="M20 7v5h-5M4 17v-5h5" /><path d="M6.1 6.1A8 8 0 0 1 20 12M4 12a8 8 0 0 0 13.9 5.9" /></>,
  trash: <><path d="M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7" /></>,
  unplug: <><path d="m16 3-4 4m9 1-4 4M3 21l4-4m-4-7 11 11M9 9l-3 3a4 4 0 0 0 6 6l3-3M3 3l18 18" /></>,
  plug: <path d="m16 3-4 4m9 1-4 4M3 21l5-5m0-8 8 8m-6-10 8 8-4 4a4 4 0 0 1-8-8z" />,
  folder: <path d="M3 7V4h6l3 3h9v13H3z" />,
  command: <path d="M9 9V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3z" />,
  keyboard: <><rect x="2" y="5" width="20" height="14" rx="2" /><path d="M6 9h.01M10 9h.01M14 9h.01M18 9h.01M6 12h.01M10 12h.01M14 12h.01M18 12h.01M7 16h10" /></>,
  check: <path d="m5 12 4 4L19 6" />,
  arrow_right: <path d="M4 12h16m-6-6 6 6-6 6" />,
  minus: <path d="M5 12h14" />,
  stop: <rect x="6" y="6" width="12" height="12" rx="1" />,
  copy: <><path d="M8 5V3h13v13h-3" /><rect x="3" y="8" width="13" height="13" rx="1" /></>,
  settings: <><path d="m9 3-1 3-3 1-2 4 2 2v4l4 2 3-1 3 1 4-2v-4l2-2-2-4-3-1-1-3z" /><circle cx="12" cy="11" r="3" /></>,
  search: <><circle cx="10" cy="10" r="6" /><path d="m15 15 6 6" /></>,
  more: <path d="M5 12h.01M12 12h.01M19 12h.01" />,
  panel_left: <><rect x="3" y="3" width="18" height="18" rx="1" /><path d="M9 3v18" /></>,
} satisfies Record<string, ReactNode>;

export type IconName = keyof typeof paths;

interface Props {
  name: IconName;
  size?: number;
  class_name?: string;
}

/** Decorative icons inherit their accessible name from the containing control. */
export function Icon({ name, size = 16, class_name }: Props) {
  return (
    <svg
      className={class_name}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {paths[name]}
    </svg>
  );
}
