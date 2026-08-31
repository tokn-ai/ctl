import type { AttachmentViewState } from "../../lib/types";
import { createStatusItems } from "./statusItems";

interface StatusBarProps {
  state: AttachmentViewState;
}

export function StatusBar({ state }: StatusBarProps) {
  return (
    <footer className="status-bar" aria-label="Terminal status">
      {createStatusItems(state).map((item) => (
        <span key={item.key} className={item.className} title={item.title}>
          {item.label}
        </span>
      ))}
    </footer>
  );
}
