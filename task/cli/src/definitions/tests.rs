use super::*;
use crate::{AttachFuture, Command, ConnectFuture};
use clap::Parser;
use task_proto::{DesiredState, ExecutionMode, RunInfo, RunState};
use tokio::io::DuplexStream;

struct TestDirectory(PathBuf);

impl TestDirectory {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("task-cli-store-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    Self(root)
  }

  fn scope(&self) -> ScopeArguments {
    ScopeArguments {
      global: false,
      project: Some(self.0.clone()),
    }
  }

  fn repository(&self) -> Repository {
    Repository::new(task_store::project_path(&self.0).unwrap())
  }
}

impl Drop for TestDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

struct NoDaemon {
  local: bool,
}

impl Connector for NoDaemon {
  type Stream = DuplexStream;
  type Error = std::io::Error;

  fn connect_task(&self) -> ConnectFuture<'_, DuplexStream, Self::Error> {
    panic!("local catalog operation must not connect to taskd")
  }

  fn is_local_task_target(&self) -> bool {
    self.local
  }

  fn attach_interactive(&self, _session: String, _rmux_socket: PathBuf) -> AttachFuture<'_> {
    panic!("local catalog operation must not attach")
  }
}

fn definition(name: &str, program: &str) -> TaskDefinition {
  TaskDefinition {
    name: name.into(),
    program: program.into(),
    arguments: vec!["--original".into()],
    working_directory: None,
    execution_mode: ExecutionMode::Background,
  }
}

fn save_arguments(directory: &TestDirectory, name: &str) -> SaveArguments {
  SaveArguments {
    name: name.into(),
    scope: directory.scope(),
    definition_id: None,
    expected_revision: None,
    from_run: None,
    cwd: None,
    mode: None,
    command: vec!["program".into(), "--original".into()],
  }
}

#[tokio::test]
async fn direct_save_is_offline_and_updates_and_removal_require_matching_revisions() {
  let directory = TestDirectory::new();
  let connector = NoDaemon { local: true };
  let saved = save_record(save_arguments(&directory, "build"), &connector)
    .await
    .unwrap();
  assert_eq!(saved.definition.program, "program");
  assert_eq!(
    saved.definition.working_directory,
    Some(std::env::current_dir().unwrap().to_str().unwrap().into())
  );
  let catalog = Catalog::resolve(&directory.scope()).unwrap();
  assert_eq!(catalog.lookup("build").unwrap(), saved);

  let mut update = save_arguments(&directory, "renamed");
  update.definition_id = Some(saved.definition_id.clone());
  update.expected_revision = Some(saved.revision.clone());
  update.command = vec!["updated-program".into()];
  let updated = save_record(update, &connector).await.unwrap();
  assert_eq!(updated.definition_id, saved.definition_id);
  assert_ne!(updated.revision, saved.revision);

  let mut stale_update = save_arguments(&directory, "overwrite");
  stale_update.definition_id = Some(saved.definition_id.clone());
  stale_update.expected_revision = Some(saved.revision.clone());
  assert!(matches!(
    save_record(stale_update, &connector).await,
    Err(CommandError::Definition(DefinitionError::Store(
      StoreError::Conflict { .. }
    )))
  ));
  assert!(catalog.remove("renamed", &saved.revision).is_err());
  assert_eq!(catalog.lookup("renamed").unwrap(), updated);
  catalog.remove("renamed", &updated.revision).unwrap();
  assert!(
    directory
      .repository()
      .load()
      .unwrap()
      .definitions
      .is_empty()
  );
}

#[test]
fn project_lookup_shadows_global_but_mutations_never_fall_back_to_global() {
  let directory = TestDirectory::new();
  let project = Repository::new(directory.0.join("project.json"));
  let global = Repository::new(directory.0.join("global.json"));
  let global_build = global
    .save(
      &uuid::Uuid::new_v4().to_string(),
      None,
      definition("build", "global-build"),
    )
    .unwrap();
  let global_only = global
    .save(
      &uuid::Uuid::new_v4().to_string(),
      None,
      definition("global-only", "global-only"),
    )
    .unwrap();
  let project_build = project
    .save(
      &uuid::Uuid::new_v4().to_string(),
      None,
      definition("build", "project-build"),
    )
    .unwrap();
  let catalog = Catalog {
    repositories: vec![
      ScopedRepository {
        scope: DefinitionScope::Project {
          project_root: directory.0.clone(),
        },
        repository: project,
      },
      ScopedRepository {
        scope: DefinitionScope::Global,
        repository: global,
      },
    ],
  };
  assert_eq!(catalog.lookup("build").unwrap(), project_build);
  assert_eq!(
    catalog.lookup(&global_build.definition_id).unwrap(),
    global_build
  );
  assert_eq!(catalog.lookup("global-only").unwrap(), global_only);
  assert!(matches!(
    catalog.remove("global-only", &global_only.revision),
    Err(DefinitionError::NotFoundInWriteScope(_))
  ));
  assert_eq!(catalog.lookup("global-only").unwrap(), global_only);
}

fn task_with_historical_run(snapshot: Option<TaskDefinition>) -> TaskInfo {
  TaskInfo {
    task_id: "task-id".into(),
    definition: definition("current", "different-current-program"),
    desired_state: DesiredState::Stopped,
    active_run: None,
    last_run: Some(RunInfo {
      definition: snapshot,
      interactive: None,
      run_id: "historical-run".into(),
      state: RunState::Completed,
      started_at_ms: 1,
      ended_at_ms: Some(2),
      exit_code: Some(0),
    }),
  }
}

#[test]
fn saving_a_run_uses_its_stored_snapshot_and_never_the_current_definition() {
  let original = definition("historical", "original-program");
  let mut task = task_with_historical_run(Some(original.clone()));
  assert_eq!(
    definition_from_run(&[task.clone()], "historical-run").unwrap(),
    original
  );
  task.active_run = task.last_run.take();
  assert_eq!(
    definition_from_run(&[task], "historical-run").unwrap(),
    original
  );
  assert!(matches!(
    definition_from_run(&[task_with_historical_run(None)], "historical-run"),
    Err(DefinitionError::MissingRunSnapshot(_))
  ));
  assert!(matches!(
    definition_from_run(&[task_with_historical_run(None)], "old-run-not-retained"),
    Err(DefinitionError::RunNotRetained(_))
  ));
}

#[tokio::test]
async fn remote_local_file_commands_fail_before_accessing_the_scope_or_daemon() {
  let directory = TestDirectory::new();
  let mut arguments = save_arguments(&directory, "remote");
  arguments.scope.project = Some(directory.0.join("does-not-exist"));
  let connector = NoDaemon { local: false };
  assert!(matches!(
    save_record(arguments, &connector).await,
    Err(CommandError::Definition(DefinitionError::LocalOnly))
  ));
  for command in [
    DefinitionCommand::List {
      scope: directory.scope(),
    },
    DefinitionCommand::Show {
      selector: "missing".into(),
      scope: directory.scope(),
    },
    DefinitionCommand::Remove {
      selector: "missing".into(),
      expected_revision: "old".into(),
      scope: directory.scope(),
    },
  ] {
    assert!(matches!(
      run(command, &connector),
      Err(CommandError::Definition(DefinitionError::LocalOnly))
    ));
  }
  assert!(!task_store::project_path(&directory.0).unwrap().exists());
}

#[derive(Parser)]
struct Arguments {
  #[command(subcommand)]
  command: Command,
}

#[test]
fn parser_preserves_legacy_create_and_accepts_explicit_definition_operations() {
  for arguments in [
    vec!["create", "build", "--start", "--", "cargo", "build"],
    vec![
      "create",
      "instance",
      "--from-definition",
      "build",
      "--global",
      "--start",
    ],
    vec!["save", "build", "--global", "--", "cargo", "build"],
    vec![
      "save",
      "saved-run",
      "--from-run",
      "run-id",
      "--project",
      "/project",
    ],
    vec![
      "save",
      "build",
      "--definition-id",
      "id",
      "--expected-revision",
      "revision",
      "--",
      "cargo",
      "check",
    ],
    vec!["definitions", "list"],
    vec!["definitions", "show", "build", "--global"],
    vec![
      "definitions",
      "remove",
      "build",
      "--expected-revision",
      "revision",
    ],
  ] {
    assert!(
      Arguments::try_parse_from(std::iter::once("task").chain(arguments.clone())).is_ok(),
      "{arguments:?}"
    );
  }
}

#[test]
fn parser_rejects_implicit_overwrites_ambiguous_sources_and_scope_conflicts() {
  for arguments in [
    vec!["save", "build", "--definition-id", "id", "--", "cargo"],
    vec![
      "save",
      "build",
      "--expected-revision",
      "revision",
      "--",
      "cargo",
    ],
    vec!["save", "build", "--from-run", "run-id", "--", "cargo"],
    vec![
      "save",
      "build",
      "--from-run",
      "run-id",
      "--cwd",
      "/override",
    ],
    vec![
      "save",
      "build",
      "--from-run",
      "run-id",
      "--mode",
      "interactive",
    ],
    vec![
      "create",
      "instance",
      "--from-definition",
      "build",
      "--cwd",
      "/override",
    ],
    vec![
      "create",
      "instance",
      "--from-definition",
      "build",
      "--mode",
      "background",
    ],
    vec![
      "create",
      "instance",
      "--from-definition",
      "build",
      "--",
      "cargo",
    ],
    vec!["definitions", "list", "--global", "--project", "/project"],
    vec!["definitions", "remove", "build"],
    vec!["save", "build"],
  ] {
    assert!(
      Arguments::try_parse_from(std::iter::once("task").chain(arguments.clone())).is_err(),
      "{arguments:?}"
    );
  }
}
