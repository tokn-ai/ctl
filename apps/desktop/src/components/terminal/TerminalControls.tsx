import { useLocale } from "../../features/i18n/LocaleContext";
import { ControlPopover } from "../ui/ControlPopover";
import { Icon } from "../ui/Icon";
import type { AttachmentViewState } from "../../lib/types";
import type { ResizeControlStatus } from "../../features/attachment/componentActions";
import { InlineFeedback } from "../feedback/InlineFeedback";

interface TerminalControlsProps {
  state: AttachmentViewState;
  onToggleResizeWithWindow(): void;
  resize_control_status: ResizeControlStatus;
  onRequestResizeControl(acquire: boolean): void;
}

export function TerminalControls({
  state,
  onToggleResizeWithWindow,
  resize_control_status,
  onRequestResizeControl,
}: TerminalControlsProps) {
  const { t, formatText } = useLocale();
  const attached = state.phase === "attached";
  const resizeOwned = resize_control_status === "owned";
  const resizeActive = state.resize_with_window && resizeOwned;
  const resizeAvailable = resize_control_status === "available";
  const layout_message = state.message_kind === "layout" ? state.message : null;
  const layout_summary = !attached || resize_control_status === "unavailable" ? t("layout.unavailable")
    : resize_control_status === "held_elsewhere" ? t("layout.held_elsewhere")
    : resizeActive ? t("layout.auto_resize") : state.resize_with_window ? t("layout.waiting") : t("layout.fixed_size");
  const resizeLabel = resizeOwned ? t("layout.owned_here") : resizeAvailable ? t("layout.available")
    : resize_control_status === "held_elsewhere" ? t("layout.held_elsewhere") : t("layout.unavailable");

  return (
    <div className="terminal-controls">
      <ControlPopover label={t("layout.summary", { summary: layout_summary })} trigger_content={<Icon name="monitor" size={14} />}>
        <span aria-label={t("layout.resize_status_label")}>{t("layout.resize_status", { status: resizeLabel })}</span>
        {layout_message ? <InlineFeedback severity="warning">{formatText(layout_message)}</InlineFeedback> : null}
        <button
          type="button"
          onClick={() => onRequestResizeControl(!resizeOwned)}
          disabled={!attached || resize_control_status === "unavailable"}
          className={resizeOwned ? "active-control" : ""}
          aria-pressed={resizeOwned}
          title={resizeOwned ? t("layout.release_hint") : resizeAvailable
            ? t("layout.take_hint")
            : resize_control_status === "held_elsewhere" ? t("layout.request_hint") : t("layout.attach_hint")}
        >
          <Icon name="monitor" size={14} />
          {resizeOwned ? t("layout.release") : t("layout.take")}
        </button>
        <button
          type="button"
          onClick={onToggleResizeWithWindow}
          disabled={!attached}
          className={resizeActive ? "active-control" : ""}
          aria-pressed={state.resize_with_window}
          aria-label={state.resize_with_window ? t("layout.use_fixed") : t("layout.resize_with_window")}
          title={state.resize_with_window ? t("layout.stop_following_window") : t("layout.follow_window")}
        >
          <Icon name="monitor" size={14} />
          {state.resize_with_window ? t("layout.auto_resize") : t("layout.fixed_size")}
        </button>
      </ControlPopover>
      {layout_message ? <span className="status-item status-tone-warning" role="status" title={formatText(layout_message)}>{t("layout.attention")}</span> : null}
    </div>
  );
}
