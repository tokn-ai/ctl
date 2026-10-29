import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { HOST_COLOR_PALETTE, normalizeHostColor, randomHostColors } from "../../features/workspace/hostColor";
import "./hostColorPicker.css";

interface Props {
  host_name: string;
  color: string;
  default_color: string;
  on_change(color: string | undefined): void;
  disabled?: boolean;
  connected?: boolean;
}

export function HostColorPicker({ host_name, color, default_color, on_change, disabled = false, connected = true }: Props) {
  const [open, setOpen] = useState(false);
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const id = useId();
  const current_color = normalizeHostColor(color);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      if (!trigger.current || !panel.current) return;
      const anchor = trigger.current.getBoundingClientRect();
      const bounds = panel.current.getBoundingClientRect();
      const width = document.documentElement.clientWidth || window.innerWidth;
      const height = document.documentElement.clientHeight || window.innerHeight;
      const below = anchor.bottom + 6;
      const above = anchor.top - bounds.height - 6;
      const top = below + bounds.height <= height - 8 ? below : above;
      setPosition({
        left: Math.max(8, Math.min(anchor.left, width - bounds.width - 8)),
        top: Math.max(8, Math.min(top, height - bounds.height - 8)),
      });
    };
    place();
    const selected = panel.current?.querySelector<HTMLButtonElement>('[aria-pressed="true"]');
    (selected ?? panel.current?.querySelector<HTMLButtonElement>("button"))?.focus();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent | FocusEvent) => {
      if (event.target instanceof Node
        && !panel.current?.contains(event.target)
        && !trigger.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("pointerdown", dismiss);
    document.addEventListener("focusin", dismiss);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      document.removeEventListener("focusin", dismiss);
    };
  }, [open]);

  const choose = (next_color: string | undefined) => {
    on_change(next_color);
    setOpen(false);
    trigger.current?.focus();
  };
  const swatch = (name: string, swatch_color: string) => (
    <button
      key={swatch_color}
      className="host-color-swatch"
      type="button"
      aria-label={name}
      title={`${name} · ${swatch_color}`}
      aria-pressed={current_color === swatch_color}
      onClick={() => choose(swatch_color)}
    >
      <span style={{ background: swatch_color }} aria-hidden="true" />
    </button>
  );

  return <>
    <button
      ref={trigger}
      className="host-color-trigger"
      type="button"
      aria-label={`Change color for ${host_name}`}
      aria-description={connected ? "Connected" : "Not connected"}
      title={`Change color for ${host_name}`}
      aria-haspopup="dialog"
      aria-expanded={open}
      aria-controls={open ? id : undefined}
      data-connected={connected}
      disabled={disabled}
      onClick={() => {
        if (!open) setSuggestions(randomHostColors());
        setOpen(!open);
      }}
    >
      <span className="host-color-dot" style={{ color, background: connected ? "currentColor" : "transparent" }} aria-hidden="true" />
    </button>
    {open && !disabled && createPortal(
      <div
        ref={panel}
        id={id}
        className="host-color-popover"
        role="dialog"
        aria-label={`Color for ${host_name}`}
        style={position}
        onKeyDown={(event) => {
          if (event.key !== "Escape") return;
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
          trigger.current?.focus();
        }}
      >
        <div className="host-color-swatches" role="group" aria-label="Preset colors">
          {HOST_COLOR_PALETTE.map(({ name, color: preset }) => swatch(name, preset))}
        </div>
        <div className="host-color-random-heading">
          <span>Random</span>
          <button type="button" onClick={() => setSuggestions(randomHostColors())}>Shuffle</button>
        </div>
        <div className="host-color-swatches host-color-suggestions" role="group" aria-label="Random colors">
          {suggestions.map((suggestion) => swatch(`Color ${suggestion}`, suggestion))}
        </div>
        <button className="host-color-reset" type="button" onClick={() => choose(undefined)} title="Reset to the default host color">
          <span className="host-color-dot" style={{ background: default_color }} aria-hidden="true" />
          Reset
        </button>
      </div>,
      document.body,
    )}
  </>;
}
