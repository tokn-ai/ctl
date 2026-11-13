import type { AttachmentViewState, NotificationAction, SshConnectionTarget } from "../../lib/types";
import { COMMAND_IDS } from "../commands/commandIds";
import { sessionKey, targetKey, targetLabel } from "../targets/targets";
import { attachmentFeedback } from "../attachment/attachmentFeedback";
import type { NotificationStore } from "./NotificationStore";

interface AttachmentSource {
  key: string;
  state: AttachmentViewState;
  connection_attempt: number;
  reconnect(): Promise<void>;
}

function authenticationTarget(state?: AttachmentViewState): SshConnectionTarget | undefined {
  const target = state?.session?.target;
  if (target?.kind === "ssh" && state?.error_code === "ssh_authentication_required" &&
      ["error", "disconnected", "retry_wait"].includes(state.phase)) return target;
}

/** Window-local attachment owners, including background tabs and split panes. */
export class AttachmentNotifications {
  private owners = new Map<string, AttachmentSource>();
  private owner_order = new Map<string, number>();
  private sources = new Map<string, string>();
  private next_owner = 0;

  constructor(
    private readonly store: NotificationStore,
    private readonly windows_client = /^Win/i.test(globalThis.navigator?.platform ?? ""),
  ) {}

  createOwner(): string {
    const order = ++this.next_owner;
    const owner = `attachment-owner-${order}`;
    this.owner_order.set(owner, order);
    return owner;
  }

  report(owner: string, state: AttachmentViewState, reconnect: () => Promise<void>, connection_attempt = 0): void {
    const session = state.session;
    const key = session ? JSON.stringify([sessionKey(session), session.terminal_id ?? null]) : null;
    const order = this.owner_order.get(owner);
    if (order === undefined) return;
    const current_owner = key ? this.sources.get(key) : undefined;
    if (current_owner && (this.owner_order.get(current_owner) ?? 0) > order) return;
    const previous = this.owners.get(owner);
    if (previous?.key !== key) {
      // An open response can resolve the root pane's identity for the first
      // time. Its earlier error still belongs to this owner's successful open.
      const previous_session = previous?.state.session;
      const root_refined = previous_session && session &&
        sessionKey(previous_session) === sessionKey(session) &&
        previous_session.terminal_id == null && session.terminal_id != null;
      if (previous && root_refined && state.phase === "attached" && this.sources.get(previous.key) === owner) {
        this.store.resolve(`attachment:${previous.key}`);
      }
      this.release(owner);
    }
    if (!session || !key) return;
    if (this.owners.get(owner)?.connection_attempt !== connection_attempt) {
      this.store.report(`attachment:${key}`, null);
    }
    this.owners.set(owner, { key, state, reconnect, connection_attempt });
    if (this.sources.get(key) !== owner) {
      // A new owner is a new open attempt, even if the previous one failed identically.
      this.store.report(`attachment:${key}`, null);
      this.store.report(`session_notice:${key}`, null);
      this.store.report(`history:${key}`, null);
      this.sources.set(key, owner);
    }
    const source = `${targetLabel(session.target)} · ${session.name}${session.terminal_id ? ` · ${session.terminal_id}` : ""}`;
    const authentication = authenticationTarget(state);
    const local_state = authentication || ["attachment_cancelled", "ssh_host_disconnected", "session_not_found"].includes(state.error_code ?? "");
    if (local_state) {
      const source_key = `attachment:${key}`;
      this.store.report(source_key, null);
      const previous_notice = this.store.snapshot().entries.find((entry) => entry.source_key === source_key);
      if (previous_notice?.toast_visible) this.store.hide(previous_notice.id);
    } else if (state.phase === "connecting" || state.phase === "reconnecting") {
      // Retain the last failure's identity throughout an automatic retry.
      this.store.setActions(`attachment:${key}`, []);
    } else {
      const failure = ["error", "disconnected", "retry_wait"].includes(state.phase);
      const feedback = attachmentFeedback(state, this.windows_client);
      const actions: NotificationAction[] = feedback.action?.kind === "recover_components"
        ? [{ label: feedback.action.label,
          command_id: COMMAND_IDS.recoverSessionComponents, args: { value: owner } }]
        : feedback.action?.kind === "connect_host" && session.target.kind === "ssh"
          ? [{ label: feedback.action.label, command_id: COMMAND_IDS.connectHost, args: { target_key: targetKey(session.target) } }]
          : feedback.action?.kind === "reconnect"
            ? [{ label: feedback.action.label, command_id: COMMAND_IDS.reconnectNotificationAttachment, args: { value: owner } }]
            : [];
      if (state.phase === "attached" || state.phase === "ended") this.store.resolve(`attachment:${key}`);
      if (failure && feedback.message) {
        this.store.report(`attachment:${key}`, {
          severity: "error",
          title: "Session connection failed",
          message: feedback.message, source, actions,
        }, { preserve_attention: true });
      }
    }
    const notice_key = `session_notice:${key}`;
    if (state.phase === "attached" && state.message && !state.message_kind) {
      this.store.report(notice_key, { severity: "warning", title: "Session update", message: state.message, source }, { preserve_attention: true });
    } else if (state.phase === "attached" || state.phase === "ended") this.store.resolve(notice_key);
    const history_key = `history:${key}`;
    if (state.history_loading) {
      // Loading is progress, not a new failure or proof of recovery. Keep the
      // occurrence and its deduplication while removing its visible toast.
      const warning = this.store.snapshot().entries.find((entry) => entry.source_key === history_key);
      if (warning?.toast_visible) this.store.hide(warning.id);
    } else if (state.history_gap) {
      this.store.report(history_key, {
        severity: "warning", title: "Earlier output unavailable", source,
        message: "Some earlier output is unavailable in this view.",
      }, { toast: false });
    } else if (state.phase === "attached" || state.phase === "ended") {
      this.store.resolve(history_key);
    }
  }

  remove(owner: string): void {
    this.owner_order.delete(owner);
    this.release(owner);
  }

  private release(owner: string): void {
    const entry = this.owners.get(owner);
    this.owners.delete(owner);
    if (!entry || this.sources.get(entry.key) !== owner) return;
    this.sources.delete(entry.key);
    this.store.report(`attachment:${entry.key}`, null);
    this.store.report(`session_notice:${entry.key}`, null);
    this.store.report(`history:${entry.key}`, null);
  }

  private current(owner?: string): AttachmentSource | undefined {
    const entry = owner ? this.owners.get(owner) : undefined;
    return entry && this.sources.get(entry.key) === owner ? entry : undefined;
  }

  recoverySession(owner?: string) {
    const state = this.current(owner)?.state;
    return state && attachmentFeedback(state, this.windows_client).action?.kind === "recover_components"
      ? state.session : null;
  }

  canReconnect(owner?: string): boolean {
    const state = this.current(owner)?.state;
    return !!state && attachmentFeedback(state, this.windows_client).action?.kind === "reconnect";
  }

  async reconnect(owner?: string): Promise<void> {
    const entry = this.current(owner);
    if (!entry || !this.canReconnect(owner)) return;
    // An explicit retry is a new attempt. Automatic retries never call this path.
    this.store.report(`attachment:${entry.key}`, null);
    await entry.reconnect();
  }
}
