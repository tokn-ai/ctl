import type { ComponentReconnectResult, ComponentSessionsReset, SessionSummary } from "../../lib/types";

interface AttachmentControl {
  attachmentId(): string | null;
  session(): SessionSummary | null;
  reconnect(expected_id: string): Promise<string | null>;
  reset(): void;
}

// Each webview has its own module registry. Every root, background tab, and
// split pane registers here so a native window-scoped event reaches all owners.
const controls = new Set<AttachmentControl>();

export function registerAttachmentControl(control: AttachmentControl): () => void {
  controls.add(control);
  return () => { controls.delete(control); };
}

export async function reconnectComponentAttachments(attachment_ids: readonly string[]): Promise<ComponentReconnectResult[]> {
  return Promise.all([...new Set(attachment_ids)].map(async (attachment_id) => {
    const owner = [...controls].find((control) => control.attachmentId() === attachment_id);
    if (!owner) return { attachment_id, replacement_attachment_id: null, error: "This attachment is no longer open in this window." };
    try {
      const replacement_attachment_id = await owner.reconnect(attachment_id);
      return { attachment_id, replacement_attachment_id, error: replacement_attachment_id ? null : "The attachment could not be reconnected or was replaced by another action." };
    } catch (failure) {
      return { attachment_id, replacement_attachment_id: null, error: failure instanceof Error ? failure.message : "Could not reconnect this attachment." };
    }
  }));
}

export function componentResetMatches(event: ComponentSessionsReset, session: SessionSummary | null, attachment_id: string | null): boolean {
  if (!session) return false;
  if (event.scope === "local") return session.target.kind === "local";
  return session.target.kind === "ssh" && (
    (event.remote_id != null && session.target.remote_info?.remote_id === event.remote_id) ||
    (session.target.host_id !== undefined && event.host_ids.includes(session.target.host_id)) ||
    (attachment_id !== null && event.attachment_ids.includes(attachment_id))
  );
}

export function resetComponentAttachments(event: ComponentSessionsReset): SessionSummary[] {
  const affected: SessionSummary[] = [];
  for (const control of controls) {
    const session = control.session();
    if (!componentResetMatches(event, session, control.attachmentId())) continue;
    affected.push(session!);
    control.reset();
  }
  return affected;
}
