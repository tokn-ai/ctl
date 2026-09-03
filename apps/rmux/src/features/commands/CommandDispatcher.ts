import type { AppCommand, CommandArguments } from "./types";

export interface CommandScope {
  commands: readonly AppCommand[];
  /** The palette may dispatch app commands; other dialogs are isolated. */
  allow_app_commands?: boolean;
  redirects?: Readonly<Record<string, string>>;
}

/** One execution policy for keyboard, native menus, palette, and pointer actions. */
export class CommandDispatcher {
  private commands: readonly AppCommand[] = [];
  private available = false;
  private scopes = new Map<symbol, CommandScope>();
  private root = Symbol("app_commands");
  private busy = new Map<symbol, Set<string>>();
  private listeners = new Set<() => void>();
  private revision = 0;
  private onError: (error: unknown) => void = () => {};

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  snapshot = () => this.revision;

  update(
    commands: readonly AppCommand[],
    available: boolean,
    onError: (error: unknown) => void,
  ) {
    this.commands = commands;
    this.available = available;
    this.onError = onError;
    this.changed();
  }

  setScope(token: symbol, scope: CommandScope) {
    this.scopes.set(token, scope);
    this.changed();
  }

  removeScope(token: symbol) {
    this.scopes.delete(token);
    this.changed();
  }

  private activeScope(): [symbol, CommandScope] | undefined {
    const scopes = [...this.scopes.entries()];
    return scopes[scopes.length - 1];
  }

  private resolve(
    id: string,
  ): { owner: symbol; command: AppCommand } | undefined {
    const active = this.activeScope();
    const scope = active?.[1];
    const scopedId = scope?.redirects?.[id] ?? id;
    const scoped = scope?.commands.find((command) => command.id === scopedId);
    if (scoped && active) return { owner: active[0], command: scoped };
    if (!this.available || (scope && !scope.allow_app_commands))
      return undefined;
    const command = this.commands.find((command) => command.id === id);
    return command ? { owner: this.root, command } : undefined;
  }

  canExecute(id: string, args: CommandArguments = {}): boolean {
    const resolved = this.resolve(id);
    return Boolean(
      resolved &&
        !this.busy.get(resolved.owner)?.has(resolved.command.id) &&
        (resolved.command.isEnabled?.(args) ?? resolved.command.enabled),
    );
  }

  /** Resolves again at execution time, never trusts a stale palette descriptor. */
  execute = (id: string, args: CommandArguments = {}): boolean => {
    const resolved = this.resolve(id);
    if (!resolved || !this.canExecute(id, args)) return false;
    const { command, owner } = resolved;
    const busy = this.busy.get(owner) ?? new Set<string>();
    this.busy.set(owner, busy);
    busy.add(command.id);
    const finish = () => {
      busy.delete(command.id);
      if (busy.size === 0) this.busy.delete(owner);
    };
    try {
      const result = command.run(args);
      if (result instanceof Promise) {
        void result.catch(this.onError).finally(() => {
          finish();
          this.changed();
        });
      } else {
        finish();
      }
    } catch (error) {
      finish();
      this.onError(error);
    }
    this.changed();
    return true;
  };

  /** Includes blocked app bindings so keyboard handling can consume them safely. */
  effectiveCommands(): AppCommand[] {
    const scope = this.activeScope()?.[1];
    const commands = new Map(
      this.commands.map((command) => [command.id, command]),
    );
    for (const command of scope?.commands ?? [])
      commands.set(command.id, command);
    return [...commands.values()].map((command) => ({
      ...command,
      enabled: this.canExecute(command.id),
    }));
  }

  private changed() {
    this.revision += 1;
    for (const listener of this.listeners) listener();
  }
}
