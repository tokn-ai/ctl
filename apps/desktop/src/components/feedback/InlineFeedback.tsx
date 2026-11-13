import type { ReactNode } from "react";
import type { NotificationSeverity } from "../../lib/types";
import "./feedback.css";

/** Feedback stays beside the content or action that owns it. */
export function InlineFeedback({ severity = "error", children, action, className = "" }: {
  severity?: NotificationSeverity;
  children: ReactNode;
  action?: { label: string; on_click(): void; disabled?: boolean };
  className?: string;
}) {
  return <div className={`inline-feedback inline-feedback-${severity} ${className}`.trim()} role={severity === "error" ? "alert" : "status"}>
    <div className="inline-feedback-content">{children}</div>
    {action ? <button type="button" disabled={action.disabled} onClick={action.on_click}>{action.label}</button> : null}
  </div>;
}
