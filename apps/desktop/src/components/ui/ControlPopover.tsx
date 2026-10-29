import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { Icon } from "./Icon";
import "./controlPopover.css";

interface Props {
  label: string;
  trigger_content?: ReactNode;
  placement?: "above" | "below" | "right";
  close_on_select?: boolean;
  children: ReactNode;
}

/** A nonmodal control group: Escape and outside interaction dismiss it without trapping focus. */
export function ControlPopover({ label, trigger_content, placement = "above", close_on_select = false, children }: Props) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const id = useId();
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent | FocusEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("pointerdown", dismiss);
    document.addEventListener("focusin", dismiss);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      document.removeEventListener("focusin", dismiss);
    };
  }, [open]);
  return (
    <div className="control-popover" data-placement={placement} ref={root} onKeyDown={(event) => {
      if (event.key === "Escape" && open) {
        event.stopPropagation();
        setOpen(false);
        trigger.current?.focus();
      }
    }}>
      <button ref={trigger} type="button" aria-label={label} title={label} aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>
        {trigger_content ?? <>{label}<Icon name="chevron_down" size={12} /></>}
      </button>
      {open && <div id={id} className="control-popover-content" role="group" aria-label={label} onClick={(event) => {
        if (close_on_select && event.target instanceof Element && event.target.closest("button:not(:disabled)")) setOpen(false);
      }}>{children}</div>}
    </div>
  );
}
