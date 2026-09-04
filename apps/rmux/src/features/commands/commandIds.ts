export const COMMAND_IDS = {
  showPalette: "view.show_command_palette",
  addHost: "host.add",
  addExistingSession: "session.add_existing",
  forgetSession: "session.forget",
  newShell: "session.new_shell",
  newTab: "tab.new_shell_here",
  refreshSessions: "session.refresh",
  nextTab: "tab.next",
  previousTab: "tab.previous",
  disconnect: "session.disconnect",
  close: "session.close",
  toggleInput: "terminal.toggle_input",
  toggleResize: "terminal.toggle_resize_with_window",
  reconnect: "terminal.reconnect",
  focus: "terminal.focus",
  restartDaemon: "daemon.restart",
  restartTaskDaemon: "taskd.restart",
  selectSession: "session.select",
  connectHost: "host.connect",
  removeHost: "host.remove",
  saveWorkspace: "workspace.save",
  configureKeybindings: "settings.configure_keybindings",
  reloadKeybindings: "settings.reload_keybindings",
} as const;

export const QUICK_INPUT_IDS = {
  accept: "quick_input.accept",
  cancel: "quick_input.cancel",
  back: "quick_input.back",
} as const;

export const CONFIGURABLE_COMMAND_IDS: readonly string[] = [
  ...Object.values(COMMAND_IDS),
  ...Object.values(QUICK_INPUT_IDS),
];
