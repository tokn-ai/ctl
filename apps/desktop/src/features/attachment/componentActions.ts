import type { ComponentReconnectResult, ComponentSessionsReset, DividerResize, ResizeDirection, SessionSummary, SessionView } from "../../lib/types";
import { sameSession } from "../targets/targets";

interface AttachmentControl {
  attachmentId(): string | null;
  session(): SessionSummary | null;
  reconnect(expected_id: string): Promise<string | null>;
  reset(): void;
  layoutOwned(): boolean;
  setViewZoom(terminal_id: string | null): Promise<void>;
  resizeDivider(divider: DividerResize, request_id: string): Promise<void>;
  resizePane(terminal_id: string, direction: ResizeDirection, amount: number, request_id: string): Promise<void>;
}

// Each webview has its own module registry. Every root, background tab, and
// split pane registers here so a native window-scoped event reaches all owners.
const controls = new Set<AttachmentControl>();

type SessionViewChange = {
  session: SessionSummary;
  attachment_id: string;
} & ({ view: SessionView; error?: never } | { error: string; view?: never });
const view_listeners = new Set<(event: SessionViewChange) => void>();

const owner_listeners = new Set<() => void>();
export function subscribeLayoutOwners(listener: () => void): () => void {
  owner_listeners.add(listener);
  return () => { owner_listeners.delete(listener); };
}
export function publishLayoutOwnerChange(): void {
  for (const listener of owner_listeners) listener();
}
export function sessionLayoutOwned(session: SessionSummary | null): boolean {
  return Boolean(session && layoutOwner(session));
}
function layoutOwner(session: SessionSummary): AttachmentControl | undefined {
  return [...controls].find((control) => control.attachmentId() && sameSession(control.session(), session) && control.layoutOwned());
}

export function subscribeSessionViews(listener: (event: SessionViewChange) => void): () => void {
  view_listeners.add(listener);
  return () => { view_listeners.delete(listener); };
}

export function publishSessionView(event: SessionViewChange): void {
  for (const listener of view_listeners) listener(event);
}

/** Keep resize ownership independent from the pane that currently has focus. */
export async function setSessionViewZoom(
  session: SessionSummary,
  terminal_id: string | null,
  baseline: Pick<SessionView, "view_id" | "revision" | "zoomed_terminal_id">,
): Promise<SessionView> {
  const owner = [...controls].find((control) =>
    control.attachmentId() && sameSession(control.session(), session) && control.layoutOwned(),
  );
  if (!owner) throw new Error("Take resize control to zoom panes.");
  const attachment_id = owner.attachmentId()!;
  const baseline_revision = BigInt(baseline.revision);
  const changed = baseline.zoomed_terminal_id !== terminal_id;
  return new Promise<SessionView>((resolve, reject) => {
    let settled = false;
    const finish = (view: SessionView | null, failure?: unknown) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      stop();
      if (view) resolve(view);
      else reject(failure);
    };
    const stop = subscribeSessionViews((event) => {
      if (event.attachment_id !== attachment_id || !sameSession(event.session, session)) return;
      if (event.error !== undefined) finish(null, new Error(event.error));
      else if (event.view.session_id === session.session_id &&
        event.view.view_id === baseline.view_id && event.view.zoomed_terminal_id === terminal_id &&
        (changed ? BigInt(event.view.revision) > baseline_revision : BigInt(event.view.revision) >= baseline_revision)) {
        finish(event.view);
      }
    });
    const timer = setTimeout(() => finish(null, new Error("The server did not confirm the pane zoom change. Reconnect and try again.")), 5000);
    try {
      void owner.setViewZoom(terminal_id).catch((failure) => finish(null, failure));
    } catch (failure) {
      finish(null, failure);
    }
  });
}

type PaneResizeResult = {
  session: SessionSummary;
  attachment_id: string;
  request_id: string;
} & ({ view: SessionView; error: null } | { view: null; error: { code: string; message: string } });
const resize_listeners = new Set<(event: PaneResizeResult) => void>();

export function publishPaneResizeResult(event: PaneResizeResult): void {
  if (event.view) publishSessionView({ session: event.session, attachment_id: event.attachment_id, view: event.view });
  for (const listener of resize_listeners) listener(event);
}

/** Wait for this exact operation, including a confirmed minimum-size no-op. */
export async function resizeSessionPane(
  session: SessionSummary,
  terminal_id: string,
  direction: ResizeDirection,
  amount: number,
): Promise<SessionView> {
  return confirmResize(session, (owner, request_id) => owner.resizePane(terminal_id, direction, amount, request_id));
}

export async function resizeSessionDivider(session: SessionSummary, divider: DividerResize): Promise<SessionView> {
  return confirmResize(session, (owner, request_id) => owner.resizeDivider(divider, request_id));
}

function confirmResize(
  session: SessionSummary,
  send: (owner: AttachmentControl, request_id: string) => Promise<void>,
): Promise<SessionView> {
  const owner = layoutOwner(session);
  if (!owner) return Promise.reject(new Error("Take resize control to resize panes."));
  const attachment_id = owner.attachmentId()!;
  const request_id = crypto.randomUUID();
  return new Promise<SessionView>((resolve, reject) => {
    let settled = false;
    const finish = (view: SessionView | null, failure?: unknown) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resize_listeners.delete(listener);
      if (view) resolve(view);
      else reject(failure);
    };
    const listener = (event: PaneResizeResult) => {
      if (event.attachment_id !== attachment_id || event.request_id !== request_id || !sameSession(event.session, session)) return;
      if (event.error) finish(null, new Error(event.error.message));
      else if (event.view.session_id === session.session_id) finish(event.view);
    };
    resize_listeners.add(listener);
    const timer = setTimeout(() => finish(null, new Error("The server did not confirm the pane resize. Reconnect and try again.")), 5000);
    try {
      void send(owner, request_id).catch((failure) => finish(null, failure));
    } catch (failure) { finish(null, failure); }
  });
}

export function registerAttachmentControl(control: AttachmentControl): () => void {
  controls.add(control);
  publishLayoutOwnerChange();
  return () => { controls.delete(control); publishLayoutOwnerChange(); };
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
