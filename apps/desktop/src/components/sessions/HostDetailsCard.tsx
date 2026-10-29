import {
  Fragment, useCallback, useEffect, useId, useLayoutEffect, useRef, useState,
  type FocusEventHandler, type KeyboardEventHandler, type PointerEventHandler, type ReactNode, type RefCallback,
} from "react";
import { createPortal } from "react-dom";
import "./hostDetailsCard.css";

export interface HostDetailsTriggerProps {
  ref: RefCallback<HTMLElement>;
  onPointerEnter: PointerEventHandler<HTMLElement>;
  onPointerLeave: PointerEventHandler<HTMLElement>;
  onFocus: FocusEventHandler<HTMLElement>;
  onBlur: FocusEventHandler<HTMLElement>;
  onKeyDown: KeyboardEventHandler<HTMLElement>;
  "aria-describedby"?: string;
}

interface Props {
  host_name: string;
  label: string;
  tone: "normal" | "muted" | "warning" | "error";
  description?: string;
  details: readonly { label: string; value: string }[];
  children(trigger_props: HostDetailsTriggerProps): ReactNode;
}

/** Whether the anchor still intersects the viewport and its scrolling ancestors. */
function isVisible(anchor: HTMLElement, bounds: DOMRect, width: number, height: number): boolean {
  if (!anchor.isConnected || bounds.right <= 0 || bounds.bottom <= 0 || bounds.left >= width || bounds.top >= height) return false;
  for (let parent = anchor.parentElement; parent; parent = parent.parentElement) {
    const style = window.getComputedStyle(parent);
    const clips_x = /auto|scroll|hidden|clip/.test(style.overflowX || style.overflow);
    const clips_y = /auto|scroll|hidden|clip/.test(style.overflowY || style.overflow);
    if (!clips_x && !clips_y) continue;
    const clip = parent.getBoundingClientRect();
    if (clips_x && (bounds.right <= clip.left || bounds.left >= clip.right)) return false;
    if (clips_y && (bounds.bottom <= clip.top || bounds.top >= clip.bottom)) return false;
  }
  return true;
}

/** A hoverable, noninteractive tooltip. The render prop adds no header wrapper. */
export function HostDetailsCard({ host_name, label, tone, description, details, children }: Props) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const trigger = useRef<HTMLElement | null>(null);
  const card = useRef<HTMLDivElement>(null);
  const trigger_hovered = useRef(false);
  const card_hovered = useRef(false);
  const focused = useRef(false);
  const dismissed = useRef(false);
  const open_timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const close_timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const id = useId();

  const clearTimers = useCallback(() => {
    if (open_timer.current !== null) clearTimeout(open_timer.current);
    if (close_timer.current !== null) clearTimeout(close_timer.current);
    open_timer.current = null;
    close_timer.current = null;
  }, []);

  const close = useCallback((suppress = false) => {
    clearTimers();
    if (suppress) dismissed.current = trigger_hovered.current || card_hovered.current || focused.current;
    card_hovered.current = false;
    setOpen(false);
  }, [clearTimers]);

  const triggerRef = useCallback<RefCallback<HTMLElement>>((node) => {
    trigger.current = node;
    if (!node) {
      trigger_hovered.current = false;
      focused.current = false;
      close();
    }
  }, [close]);

  useEffect(() => clearTimers, [clearTimers]);

  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      if (!trigger.current || !card.current) return;
      const anchor = trigger.current.getBoundingClientRect();
      const bounds = card.current.getBoundingClientRect();
      const width = document.documentElement.clientWidth || window.innerWidth;
      const height = document.documentElement.clientHeight || window.innerHeight;
      if (!isVisible(trigger.current, anchor, width, height)) {
        close(true);
        return;
      }
      // Leave room for the row's connection/settings actions beside the name.
      const row = trigger.current.closest<HTMLElement>(".host-group-header")?.getBoundingClientRect() ?? anchor;
      const right = row.right + 8;
      const left = right + bounds.width <= width - 8 ? right : row.left - bounds.width - 8;
      const next = {
        top: Math.max(8, Math.min(anchor.top, height - bounds.height - 8)),
        left: Math.max(8, Math.min(left, width - bounds.width - 8)),
      };
      setPosition((current) => current.top === next.top && current.left === next.left ? current : next);
    };
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(place);
    if (trigger.current) observer?.observe(trigger.current);
    if (card.current) observer?.observe(card.current);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
      observer?.disconnect();
    };
  }, [open, host_name, label, description, details, close]);

  useEffect(() => {
    if (!open) return;
    const dismissOutside = (event: PointerEvent | FocusEvent) => {
      if (event.target instanceof Node
        && !trigger.current?.contains(event.target)
        && !card.current?.contains(event.target)) close(true);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      close(true);
    };
    document.addEventListener("pointerdown", dismissOutside);
    document.addEventListener("focusin", dismissOutside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", dismissOutside);
      document.removeEventListener("focusin", dismissOutside);
      document.removeEventListener("keydown", escape);
    };
  }, [open, close]);

  const leave = () => {
    clearTimers();
    if (focused.current || trigger_hovered.current || card_hovered.current) return;
    // Bridge the small pointer gap between the name and the portaled card.
    close_timer.current = setTimeout(() => close(), 150);
  };

  const trigger_props: HostDetailsTriggerProps = {
    ref: triggerRef,
    "aria-describedby": open ? id : undefined,
    onPointerEnter: (event) => {
      if (event.pointerType === "touch") return;
      trigger_hovered.current = true;
      clearTimers();
      if (dismissed.current) return;
      open_timer.current = setTimeout(() => {
        open_timer.current = null;
        if (trigger.current?.isConnected && !dismissed.current) setOpen(true);
      }, 250);
    },
    onPointerLeave: () => {
      trigger_hovered.current = false;
      dismissed.current = false;
      leave();
    },
    onFocus: () => {
      focused.current = true;
      dismissed.current = false;
      clearTimers();
      setOpen(true);
    },
    onBlur: (event) => {
      if (event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget)) return;
      focused.current = false;
      if (event.relatedTarget instanceof Node && card.current?.contains(event.relatedTarget)) return;
      // Selecting noninteractive card text can blur the name without focusing another control.
      if (event.relatedTarget === null && card_hovered.current) return;
      close(true);
    },
    onKeyDown: (event) => {
      if (event.key !== "Escape" || (!open && open_timer.current === null)) return;
      event.preventDefault();
      event.stopPropagation();
      close(true);
    },
  };

  return <>
    {children(trigger_props)}
    {open && createPortal(
      <div
        ref={card}
        id={id}
        className="host-details-card"
        role="tooltip"
        style={position}
        onPointerEnter={() => {
          card_hovered.current = true;
          clearTimers();
        }}
        onPointerLeave={() => {
          card_hovered.current = false;
          dismissed.current = false;
          leave();
        }}
      >
        <div className="host-details-card-heading">
          <strong>{host_name}</strong>
          <span data-tone={tone}>{label}</span>
        </div>
        {description && <p className="host-details-description">{description}</p>}
        {details.length > 0 && <dl>
          {details.map((detail, index) => <Fragment key={`${detail.label}:${index}`}>
            <dt>{detail.label}</dt>
            <dd>{detail.value}</dd>
          </Fragment>)}
        </dl>}
      </div>,
      document.body,
    )}
  </>;
}
