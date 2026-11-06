import type { AttachmentViewState, NotificationAction } from "../../lib/types";
import { COMMAND_IDS } from "../commands/commandIds";
import { sessionKey, targetLabel } from "../targets/targets";
import type { NotificationStore } from "./NotificationStore";

interface AttachmentSource {
  key: string;
  state: AttachmentViewState;
  connection_attempt: number;
  reconnect(): Promise<void>;
}

/** Window-local attachment owners, including background tabs and split panes. */
export class AttachmentNotifications {
  private owners = new Map<string, AttachmentSource>();
  private owner_order = new Map<string, number>();
  private sources = new Map<string, string>();
  private next_owner = 0;

  constructor(private readonly store: NotificationStore) {}

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
      this.store.resolve(`input-lease:${key}`);
      this.store.report(`history:${key}`, null);
      this.sources.set(key, owner);
    }
    const source = `${targetLabel(session.target)} · ${session.name}${session.terminal_id ? ` · ${session.terminal_id}` : ""}`;
    if (state.phase === "connecting" || state.phase === "reconnecting") {
      // Retain the last failure's identity throughout an automatic retry.
      this.store.setActions(`attachment:${key}`, []);
    } else {
      const failure = ["error", "disconnected", "retry_wait"].includes(state.phase);
      const actions: NotificationAction[] = this.recoverySession(owner)
        ? [{ label: session.target.kind === "ssh" ? "Update remote components" : "Restart local daemon",
          command_id: COMMAND_IDS.recoverSessionComponents, args: { value: owner } }]
        : this.canReconnect(owner)
          ? [{ label: "Reconnect", command_id: COMMAND_IDS.reconnectNotificationAttachment, args: { value: owner } }]
          : [];
      if (state.phase === "attached" && !state.message) this.store.resolve(`attachment:${key}`);
      this.store.report(`attachment:${key}`, state.message ? {
        severity: failure ? "error" : state.phase === "ended" ? "info" : "warning",
        title: failure ? "Session connection failed" : "Session update",
        message: state.message, source, actions,
      } : null);
    }
    const input_key = `input-lease:${key}`;
    if (state.phase === "attached" && state.input_lease_request?.status === "denied") {
      // A new denial is actionable even if the previous warning was dismissed.
      if (previous?.state.input_lease_request !== state.input_lease_request) this.store.report(input_key, null);
      this.store.report(input_key, {
        severity: "warning", title: "Input is in use", source,
        message: "Another attachment has input control. Release it there before trying again here.",
      });
    } else if (state.phase !== "attached" || !state.input_lease_request) {
      this.store.resolve(input_key);
    }
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
      });
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
    this.store.resolve(`input-lease:${entry.key}`);
    this.store.report(`history:${entry.key}`, null);
  }

  private current(owner?: string): AttachmentSource | undefined {
    const entry = owner ? this.owners.get(owner) : undefined;
    return entry && this.sources.get(entry.key) === owner ? entry : undefined;
  }

  recoverySession(owner?: string) {
    const state = this.current(owner)?.state;
    return state?.phase === "error" && state.error_code === "protocol_version_mismatch" ? state.session : null;
  }

  canReconnect(owner?: string): boolean {
    const state = this.current(owner)?.state;
    return !!state && ["error", "disconnected", "retry_wait"].includes(state.phase) && state.error_code !== "protocol_version_mismatch";
  }

  async reconnect(owner?: string): Promise<void> {
    const entry = this.current(owner);
    if (!entry || !this.canReconnect(owner)) return;
    // An explicit retry is a new attempt. Automatic retries never call this path.
    this.store.report(`attachment:${entry.key}`, null);
    await entry.reconnect();
  }
}
