import type { SessionSummary, SessionView } from "../../lib/types";
import { useDividerDrag } from "../../features/terminal/useDividerDrag";
import { viewDividers } from "../../features/terminal/viewLayout";

interface Props {
  session: SessionSummary | null;
  view: SessionView | null;
  cell: { width: number; height: number };
  enabled: boolean;
  current_view?(): SessionView | null;
  can_begin(): boolean;
  on_busy(busy: boolean): void;
  on_error(message: string | null): void;
  on_confirm(view: SessionView): void;
}

/** Keep hit areas over the daemon's gaps; only confirmed views resize panes. */
export function ViewDividers(props: Props) {
  const { view, cell, enabled } = props;
  const { handlers, preview, owned } = useDividerDrag(props);
  if (!view || view.zoomed_terminal_id) return null;
  const panes = view.panes.map((pane) => ({ ...pane, width: pane.columns, height: pane.rows, visible: true }));
  return viewDividers(view.layout, panes).map((divider, index) => {
    const extent = divider.vertical ? view.canvas_size.columns : view.canvas_size.rows;
    const position = Math.floor(divider.vertical ? divider.left : divider.top);
    const active = preview?.path === divider.path;
    return <div key={divider.path}>
      <div {...handlers(divider)} className="view-divider-handle" data-divider-path={divider.path}
        data-vertical={divider.vertical} data-active={active} role="separator"
        aria-label={`Resize pane divider ${index + 1}`} aria-orientation={divider.vertical ? "vertical" : "horizontal"}
        aria-valuenow={position} aria-valuemin={0} aria-valuemax={extent - 1}
        aria-disabled={!enabled || !owned} tabIndex={enabled && owned ? 0 : -1}
        style={{ left: divider.left * cell.width - (divider.vertical ? 4 : 0),
          top: divider.top * cell.height - (divider.vertical ? 0 : 4),
          width: divider.vertical ? 8 : divider.length * cell.width,
          height: divider.vertical ? divider.length * cell.height : 8 }} />
      {active && <div className="view-divider-preview" aria-hidden="true"
        style={{ left: divider.vertical ? (preview.position + 0.5) * cell.width : divider.left * cell.width,
          top: divider.vertical ? divider.top * cell.height : (preview.position + 0.5) * cell.height,
          width: divider.vertical ? 1 : divider.length * cell.width,
          height: divider.vertical ? divider.length * cell.height : 1 }} />}
    </div>;
  });
}
