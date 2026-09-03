import { useEffect, useRef, type KeyboardEvent, type ReactNode } from "react";

interface QuickInputFrameProps {
  title: string;
  children: ReactNode;
  onDismiss(): void;
  onKeyDown?(event: KeyboardEvent<HTMLElement>): void;
}

/** Shared positioning, cancellation, and focus containment for quick input. */
export function QuickInputFrame({
  title,
  children,
  onDismiss,
  onKeyDown,
}: QuickInputFrameProps) {
  const frameRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const frame = frameRef.current;
    if (frame && !frame.contains(document.activeElement)) frame.focus();
  }, []);

  return (
    <div
      className="command-palette-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onDismiss();
      }}
    >
      <section
        ref={frameRef}
        tabIndex={-1}
        className="command-palette"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing) return;
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            onDismiss();
          } else if (event.key === "Tab") {
            const elements = [
              ...event.currentTarget.querySelectorAll<HTMLElement>(
                'input:not(:disabled), button:not(:disabled):not([tabindex="-1"])',
              ),
            ];
            if (!elements.length) {
              event.preventDefault();
              event.currentTarget.focus();
              return;
            }
            const index = elements.indexOf(
              document.activeElement as HTMLElement,
            );
            const next =
              (index + (event.shiftKey ? -1 : 1) + elements.length) %
              elements.length;
            event.preventDefault();
            elements[next].focus();
          } else if (event.key === "Enter" && event.repeat) {
            event.preventDefault();
          } else {
            onKeyDown?.(event);
          }
        }}
      >
        {children}
      </section>
    </div>
  );
}
