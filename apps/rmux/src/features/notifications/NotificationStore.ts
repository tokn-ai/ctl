import type { AppNotification, NotificationInput } from "../../lib/types";

export const MAX_NOTIFICATIONS = 100;
export const MAX_NOTIFICATION_TOASTS = 3;

interface NotificationSnapshot {
  entries: readonly AppNotification[];
  center_open: boolean;
}

/** Window-lifetime history. No notification contents are written to disk. */
export class NotificationStore {
  private state: NotificationSnapshot = { entries: [], center_open: false };
  private listeners = new Set<() => void>();
  private reported = new Map<string, string>();
  private next_id = 0;

  constructor(private readonly now: () => number = Date.now) {}

  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  /** A dismissed error stays dismissed until its source changes or recovers. */
  report(source_key: string, input: NotificationInput | null): void {
    if (input === null) {
      this.reported.delete(source_key);
      return;
    }
    const signature = JSON.stringify([input.severity, input.title, input.message, input.source]);
    const previous = this.state.entries.find((entry) => entry.source_key === source_key);
    if (this.reported.get(source_key) === signature) {
      // Availability or context can change without being a new occurrence.
      if (previous && JSON.stringify(previous.actions) !== JSON.stringify(input.actions)) {
        this.update({ ...this.state, entries: this.state.entries.map((entry) =>
          entry === previous ? { ...entry, actions: input.actions } : entry) });
      }
      return;
    }
    this.reported.delete(source_key);
    this.reported.set(source_key, signature);
    if (this.reported.size > MAX_NOTIFICATIONS * 2) {
      this.reported.delete(this.reported.keys().next().value!);
    }
    const repeated = previous?.severity === input.severity &&
      previous.title === input.title && previous.message === input.message;
    const time = this.now();
    const notification: AppNotification = {
      ...input,
      id: previous?.id ?? `notification-${++this.next_id}`,
      source_key,
      created_at: previous?.created_at ?? time,
      updated_at: time,
      occurrence_count: repeated ? previous.occurrence_count + 1 : 1,
      read: this.state.center_open,
      toast_visible: !this.state.center_open,
    };
    let visible = 0;
    const entries = [notification, ...this.state.entries.filter((entry) => entry !== previous)]
      .slice(0, MAX_NOTIFICATIONS)
      .map((entry) => entry.toast_visible && ++visible > MAX_NOTIFICATION_TOASTS
        ? { ...entry, toast_visible: false }
        : entry);
    this.update({ ...this.state, entries });
  }

  hide(id: string): void {
    this.update({ ...this.state, entries: this.state.entries.map((entry) =>
      entry.id === id ? { ...entry, toast_visible: false } : entry) });
  }

  dismiss(id: string): void {
    this.update({ ...this.state, entries: this.state.entries.filter((entry) => entry.id !== id) });
  }

  clear(): void {
    this.update({ ...this.state, entries: [] });
  }

  setCenterOpen(center_open: boolean): void {
    this.update({
      center_open,
      entries: center_open
        ? this.state.entries.map((entry) => ({ ...entry, read: true, toast_visible: false }))
        : this.state.entries,
    });
  }

  private update(state: NotificationSnapshot): void {
    this.state = state;
    this.listeners.forEach((listener) => listener());
  }
}
