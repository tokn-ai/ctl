use super::{CommandError, Connector, Mode, connect, local_working_directory, one, unexpected};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use task_proto::{ClientMessage, TaskDefinition, TaskInfo};
use task_store::{DefinitionScope, Repository, SavedTaskDefinition, StoreError};
use thiserror::Error;

#[derive(Debug, Clone, Default, Args)]
pub struct ScopeArguments {
  /// Use the user-global definition catalog.
  #[arg(long, conflicts_with = "project")]
  pub global: bool,
  /// Use the catalog rooted at an existing project directory.
  #[arg(long, value_name = "PATH")]
  pub project: Option<PathBuf>,
}

impl ScopeArguments {
  pub(super) fn is_explicit(&self) -> bool {
    self.global || self.project.is_some()
  }
}

#[derive(Debug, Subcommand)]
pub enum DefinitionCommand {
  /// List project and global definitions with their IDs and revisions.
  List {
    #[command(flatten)]
    scope: ScopeArguments,
  },
  /// Show a definition by ID or name, preferring the current project.
  Show {
    selector: String,
    #[command(flatten)]
    scope: ScopeArguments,
  },
  /// Remove a definition from the selected scope if its revision still matches.
  Remove {
    selector: String,
    #[arg(long)]
    expected_revision: String,
    #[command(flatten)]
    scope: ScopeArguments,
  },
}

#[derive(Debug, Args)]
pub struct SaveArguments {
  pub name: String,
  #[command(flatten)]
  pub scope: ScopeArguments,
  /// Update this stable definition ID instead of creating a new definition.
  #[arg(long, requires = "expected_revision")]
  pub definition_id: Option<String>,
  /// Require the current revision when updating; obtain it with definitions show.
  #[arg(long, requires = "definition_id")]
  pub expected_revision: Option<String>,
  /// Copy the exact active or latest retained run snapshot from local taskd.
  #[arg(long, conflicts_with_all = ["command", "cwd", "mode"])]
  pub from_run: Option<String>,
  /// Capture this local directory; defaults to the caller's current directory.
  #[arg(long)]
  pub cwd: Option<String>,
  #[arg(long, value_enum)]
  pub mode: Option<Mode>,
  #[arg(last = true, required_unless_present = "from_run")]
  pub command: Vec<String>,
}

struct ScopedRepository {
  scope: DefinitionScope,
  repository: Repository,
}

struct Catalog {
  // Resolution order is project, then global. Mutations use only the first
  // repository so a missing project entry never silently changes global data.
  repositories: Vec<ScopedRepository>,
}

impl Catalog {
  fn resolve(arguments: &ScopeArguments) -> Result<Self, DefinitionError> {
    let scopes = if arguments.global {
      vec![DefinitionScope::Global]
    } else if let Some(project_root) = &arguments.project {
      vec![DefinitionScope::Project {
        project_root: project_root.clone(),
      }]
    } else {
      let current = std::env::current_dir().map_err(DefinitionError::CurrentDirectory)?;
      let mut scopes = Vec::new();
      if let Some(project_root) = task_store::discover_project(&current)? {
        scopes.push(DefinitionScope::Project { project_root });
      }
      scopes.push(DefinitionScope::Global);
      scopes
    };
    let repositories = scopes
      .into_iter()
      .map(|scope| {
        Ok(ScopedRepository {
          repository: Repository::new(scope.path()?),
          scope,
        })
      })
      .collect::<Result<_, DefinitionError>>()?;
    Ok(Self { repositories })
  }

  fn primary(&self) -> &Repository {
    &self.repositories[0].repository
  }

  fn lookup(&self, selector: &str) -> Result<SavedTaskDefinition, DefinitionError> {
    for scoped in &self.repositories {
      if let Some(definition) = find_definition(&scoped.repository, selector)? {
        return Ok(definition);
      }
    }
    Err(DefinitionError::NotFound(selector.into()))
  }

  fn remove(&self, selector: &str, expected_revision: &str) -> Result<(), DefinitionError> {
    let repository = self.primary();
    let definition = find_definition(repository, selector)?
      .ok_or_else(|| DefinitionError::NotFoundInWriteScope(selector.into()))?;
    repository.remove(&definition.definition_id, expected_revision)?;
    Ok(())
  }
}

fn find_definition(
  repository: &Repository,
  selector: &str,
) -> Result<Option<SavedTaskDefinition>, DefinitionError> {
  let definitions = repository.load()?.definitions;
  Ok(
    definitions
      .iter()
      .find(|saved| saved.definition_id == selector)
      .or_else(|| {
        definitions
          .iter()
          .find(|saved| saved.definition.name == selector)
      })
      .cloned(),
  )
}

pub(super) fn require_local<C: Connector>(connector: &C) -> Result<(), DefinitionError> {
  if connector.is_local_task_target() {
    Ok(())
  } else {
    Err(DefinitionError::LocalOnly)
  }
}

pub(super) fn lookup(
  scope: &ScopeArguments,
  selector: &str,
) -> Result<SavedTaskDefinition, DefinitionError> {
  Catalog::resolve(scope)?.lookup(selector)
}

pub(super) fn run<C: Connector>(
  command: DefinitionCommand,
  connector: &C,
) -> Result<(), CommandError> {
  require_local(connector)?;
  match command {
    DefinitionCommand::List { scope } => {
      let catalog = Catalog::resolve(&scope)?;
      println!("SCOPE\tDEFINITION_ID\tREVISION\tNAME");
      for scoped in catalog.repositories {
        let label = match scoped.scope {
          DefinitionScope::Global => "global",
          DefinitionScope::Project { .. } => "project",
        };
        for saved in scoped
          .repository
          .load()
          .map_err(DefinitionError::from)?
          .definitions
        {
          println!(
            "{label}\t{}\t{}\t{}",
            saved.definition_id, saved.revision, saved.definition.name
          );
        }
      }
    }
    DefinitionCommand::Show { selector, scope } => {
      print_saved(&lookup(&scope, &selector)?)?;
    }
    DefinitionCommand::Remove {
      selector,
      expected_revision,
      scope,
    } => {
      Catalog::resolve(&scope)?.remove(&selector, &expected_revision)?;
      println!("removed definition {selector}");
    }
  }
  Ok(())
}

pub(super) async fn save<C: Connector>(
  arguments: SaveArguments,
  connector: &C,
) -> Result<(), CommandError> {
  let saved = save_record(arguments, connector).await?;
  print_saved(&saved)?;
  Ok(())
}

pub(super) async fn save_record<C: Connector>(
  arguments: SaveArguments,
  connector: &C,
) -> Result<SavedTaskDefinition, CommandError> {
  require_local(connector)?;
  if arguments.definition_id.is_some() != arguments.expected_revision.is_some() {
    return Err(DefinitionError::RevisionRequired.into());
  }
  let catalog = Catalog::resolve(&arguments.scope)?;
  let mut definition = if let Some(run_id) = arguments.from_run {
    if !arguments.command.is_empty() || arguments.cwd.is_some() || arguments.mode.is_some() {
      return Err(DefinitionError::RunOverrides.into());
    }
    let mut stream = connect(connector).await?;
    let response = one(&mut stream, ClientMessage::ListTasks).await?;
    let task_proto::ServerMessage::TaskList { tasks } = response else {
      return Err(unexpected("task_list", &response));
    };
    definition_from_run(&tasks, &run_id)?
  } else {
    let mut command = arguments.command.into_iter();
    TaskDefinition {
      name: arguments.name.clone(),
      program: command.next().ok_or(CommandError::MissingProgram)?,
      arguments: command.collect(),
      working_directory: Some(local_working_directory(arguments.cwd.as_deref())?),
      execution_mode: arguments.mode.unwrap_or(Mode::Background).into(),
    }
  };
  definition.name = arguments.name;
  let definition_id = arguments
    .definition_id
    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
  catalog
    .primary()
    .save(
      &definition_id,
      arguments.expected_revision.as_deref(),
      definition,
    )
    .map_err(DefinitionError::from)
    .map_err(CommandError::from)
}

fn definition_from_run(
  tasks: &[TaskInfo],
  run_id: &str,
) -> Result<TaskDefinition, DefinitionError> {
  let run = tasks
    .iter()
    .flat_map(|task| task.active_run.iter().chain(task.last_run.iter()))
    .find(|run| run.run_id == run_id)
    .ok_or_else(|| DefinitionError::RunNotRetained(run_id.into()))?;
  run
    .definition
    .clone()
    .ok_or_else(|| DefinitionError::MissingRunSnapshot(run_id.into()))
}

fn print_saved(saved: &SavedTaskDefinition) -> Result<(), DefinitionError> {
  println!("{}", serde_json::to_string_pretty(saved)?);
  Ok(())
}

#[derive(Debug, Error)]
pub enum DefinitionError {
  #[error(transparent)]
  Store(#[from] StoreError),
  #[error("could not serialize saved definition: {0}")]
  Serialize(#[from] serde_json::Error),
  #[error("could not resolve current directory: {0}")]
  CurrentDirectory(#[source] std::io::Error),
  #[error("saved definition commands use local files; remove --host")]
  LocalOnly,
  #[error("--global and --project on create require --from-definition")]
  ScopeRequiresDefinition,
  #[error("updating a saved definition requires both --definition-id and --expected-revision")]
  RevisionRequired,
  #[error("--from-run cannot be combined with a command, --cwd, or --mode")]
  RunOverrides,
  #[error("saved definition {0:?} was not found in the selected scope")]
  NotFound(String),
  #[error(
    "saved definition {0:?} was not found in the write scope; use --global or --project explicitly"
  )]
  NotFoundInWriteScope(String),
  #[error("run {0:?} is not retained; only active and latest runs can be saved")]
  RunNotRetained(String),
  #[error("run {0:?} has no stored definition snapshot; its original command cannot be recovered")]
  MissingRunSnapshot(String),
}

#[cfg(test)]
mod tests;
