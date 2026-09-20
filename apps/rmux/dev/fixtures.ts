import type {
  ConnectionTarget,
  ManagedTask,
  SavedTaskDefinition,
  SessionSummary,
  ShellStateSummary,
  WorkspaceDocument,
  WorkspaceSidebarView,
} from "../src/lib/types";

export const previewTargets: ConnectionTarget[] = [
  { kind: "local" },
  {
    kind: "ssh",
    host_id: "preview-dev",
    destination: "dev-server",
    remote_info: { remote_id: "sample-dev", agent_version: "0.1.0" },
  },
  {
    kind: "ssh",
    host_id: "preview-staging",
    destination: "staging",
    remote_info: { remote_id: "sample-staging", agent_version: "0.1.0" },
  },
];

export const previewSessions: SessionSummary[] = [
  ["local-shell", "zsh", 0],
  ["local-tests", "cargo test", 0],
  ["dev-shell", "api", 1],
  ["dev-worker", "worker", 1],
  ["staging-shell", "deploy", 2],
].map(([session_id, name, host_index]) => ({
  session_id: String(session_id),
  name: String(name),
  target: previewTargets[Number(host_index)],
  status: "running",
  next_sequence: "0",
  terminal_size: { columns: 112, rows: 30, pixel_width: null, pixel_height: null },
}));

export function previewShell(session: SessionSummary): ShellStateSummary {
  const local = session.target.kind === "local";
  const cwd = local ? "/Users/developer/Projects/ctl" : "/home/developer/services/api";
  return {
    shell_type: "zsh",
    cwd,
    cwd_display: local ? "~/Projects/ctl" : "~/services/api",
    running_command: session.session_id === "local-tests" ? "cargo test --workspace" : null,
    prompt_phase: session.session_id === "local-tests" ? "running" : "at_prompt",
    tui_hint: "inline",
    revision: "1",
    observed_sequence: "0",
  };
}

export const previewDefinitions: SavedTaskDefinition[] = [
  {
    definition_id: "preview-web",
    revision: "1",
    definition: { name: "Web development", program: "pnpm", arguments: ["dev"], working_directory: "/Users/developer/Projects/ctl/apps/rmux", execution_mode: "background" },
  },
  {
    definition_id: "preview-tests",
    revision: "1",
    definition: { name: "Rust test suite", program: "cargo", arguments: ["test", "--workspace"], working_directory: "/Users/developer/Projects/ctl", execution_mode: "background" },
  },
  {
    definition_id: "preview-check",
    revision: "1",
    definition: { name: "Type check", program: "pnpm", arguments: ["check"], working_directory: "/Users/developer/Projects/ctl/apps/rmux", execution_mode: "background" },
  },
];

export const previewTasks: ManagedTask[] = previewDefinitions.map((saved, index) => {
  const run = {
    run_id: `preview-run-${index}`,
    state: index === 0 ? "running" as const : "completed" as const,
    started_at_ms: 1790000000000,
    ended_at_ms: index === 0 ? null : 1790000006200,
    exit_code: index === 0 ? null : 0,
    definition: saved.definition,
  };
  return {
    task_id: `preview-task-${index}`,
    definition: saved.definition,
    desired_state: index === 0 ? "running" : "stopped",
    active_run: index === 0 ? run : null,
    last_run: index === 0 ? null : run,
  };
});

export function previewWorkspace(view: WorkspaceSidebarView): WorkspaceDocument {
  const session_refs = previewSessions.map((session) => ({
    kind: "session" as const,
    host_id: session.target.kind === "local" ? "local" : session.target.host_id!,
    session_id: session.session_id,
  }));
  return {
    schema_version: 6,
    ssh_gateways: [],
    workspace_id: "sample-workspace",
    sidebar_view: view,
    hosts: previewTargets.map((target) => ({
      host_id: target.kind === "local" ? "local" : target.host_id!,
      target,
    })),
    sessions: previewSessions.map((session, index) => ({
      ...session_refs[index],
      name: session.name,
      last_known_cwd: previewShell(session).cwd,
      last_known_cwd_display: previewShell(session).cwd_display ?? null,
    })),
    tabs: [session_refs[0], session_refs[2], { kind: "task", host_id: "local", task_id: "preview-task-0" }],
    active_tab: view === "tasks" ? { kind: "task", host_id: "local", task_id: "preview-task-0" } : session_refs[0],
    task_definition_scope: { kind: "global" },
    task_references: previewTasks.map((task, index) => ({
      host_id: "local",
      task_id: task.task_id,
      definition_id: previewDefinitions[index].definition_id,
      definition_scope: { kind: "global" },
      applied_revision: "1",
      is_default: true,
    })),
    port_forwards: [
      { forward_id: "preview-api", host_id: "preview-dev", name: "API server", bind_address: "127.0.0.1", local_port: 8080, remote_host: "127.0.0.1", remote_port: 8080, enabled: true },
      { forward_id: "preview-db", host_id: "preview-dev", name: "PostgreSQL", bind_address: "127.0.0.1", local_port: 5432, remote_host: "127.0.0.1", remote_port: 5432, enabled: true },
      { forward_id: "preview-metrics", host_id: "preview-staging", name: "Metrics", bind_address: "127.0.0.1", local_port: 9090, remote_host: "127.0.0.1", remote_port: 9090, enabled: false },
    ],
  };
}

export function previewOutput(session: SessionSummary): string {
  if (session.session_id === "local-tests") {
    return "\x1b[32m   Compiling\x1b[0m ctl v0.1.0\r\n\x1b[32m    Finished\x1b[0m test profile in 2.48s\r\n\r\nrunning 128 tests\r\ntest attachment::reconnect_preserves_scrollback ... \x1b[32mok\x1b[0m\r\ntest workspace::restores_open_tabs ... \x1b[32mok\x1b[0m\r\ntest forwarding::resumes_after_reconnect ... \x1b[32mok\x1b[0m\r\n\r\n\x1b[32mtest result: ok.\x1b[0m 128 passed; 0 failed\r\n";
  }
  const host = session.target.kind === "local" ? "macbook" : session.target.destination;
  const cwd = previewShell(session).cwd_display;
  return [
    "\x1b[90mSample workspace · browser preview\x1b[0m",
    "",
    `\x1b[36m${cwd}\x1b[0m \x1b[90mon\x1b[0m \x1b[35mmain\x1b[0m`,
    "\x1b[32m❯\x1b[0m git status --short",
    " \x1b[33mM\x1b[0m apps/rmux/src/App.css",
    " \x1b[33mM\x1b[0m apps/rmux/src/components/sessions/SessionSidebar.tsx",
    " \x1b[33mM\x1b[0m apps/rmux/src/components/tabs/TerminalTabs.tsx",
    "",
    `\x1b[36m${cwd}\x1b[0m \x1b[90mon\x1b[0m \x1b[35mmain\x1b[0m`,
    "\x1b[32m❯\x1b[0m cargo check --workspace",
    "\x1b[32m    Checking\x1b[0m rmux-protocol v0.1.0",
    "\x1b[32m    Checking\x1b[0m ctld v0.1.0",
    "\x1b[32m    Checking\x1b[0m rmux-app v0.1.0",
    "\x1b[32m    Finished\x1b[0m dev profile [unoptimized + debuginfo] in 1.42s",
    "",
    `\x1b[36m${cwd}\x1b[0m \x1b[90mon\x1b[0m \x1b[35mmain\x1b[0m \x1b[90m· ${host}\x1b[0m`,
    "\x1b[32m❯\x1b[0m ",
  ].join("\r\n");
}
