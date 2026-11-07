use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
  name = "ctlup",
  version,
  about = "Install and update ctl without restarting running services"
)]
pub(super) struct Arguments {
  /// Select a saved ctl host by name or ID.
  #[arg(long, short = 'H', global = true)]
  pub host: Option<String>,
  /// Use a temporary SSH destination or configuration alias.
  #[arg(long, global = true, conflicts_with_all = ["host", "method"])]
  pub destination: Option<String>,
  /// Select one saved host's connection method.
  #[arg(long, global = true)]
  pub method: Option<String>,
  #[command(subcommand)]
  pub command: Command,
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
  #[command(long_flag = "component-info", hide = true)]
  ComponentInfo,
  /// Install a complete build and expose commands in ~/.tokn/ctl/bin.
  Install(UpdateArguments),
  /// Update a complete installation; running sessions keep their current owner.
  Update(UpdateArguments),
  /// Show the active installation and rollback candidate without starting services.
  Status {
    #[arg(long)]
    json: bool,
  },
  /// Restore the previous installation without restarting services.
  Rollback {
    #[arg(long)]
    json: bool,
  },
  /// Check publication metadata without downloading archives or installing anything.
  Check {
    #[arg(long, value_enum)]
    channel: Option<crate::components::Channel>,
    #[arg(long)]
    json: bool,
  },
  /// Remove unused builds, retaining active, previous and running installations.
  Prune {
    #[arg(long)]
    json: bool,
  },
  #[command(hide = true)]
  InstallArchive,
  #[command(hide = true)]
  ActivateInstalled { bundle_id: String },
}

#[derive(Debug, Args)]
pub(super) struct UpdateArguments {
  /// Saved host names or IDs; repeat or separate with commas.
  #[arg(long, value_delimiter = ',', num_args = 1..)]
  pub hosts: Vec<String>,
  /// Include this computer; the default when no host is supplied.
  #[arg(long)]
  pub local: bool,
  /// Publication channel; remembered after a successful update.
  #[arg(long, value_enum, conflicts_with = "from")]
  pub channel: Option<crate::components::Channel>,
  /// Complete build directory or archive for an explicit offline installation.
  #[arg(long, conflicts_with = "channel")]
  pub from: Option<PathBuf>,
  /// Inspect and snapshot native binaries from --from.
  #[arg(long, requires = "from")]
  pub local_build: bool,
  /// Complete signed ctld.app package and receipt for a native macOS build.
  #[arg(long, requires = "local_build")]
  pub ctld_package: Option<PathBuf>,
  #[arg(long)]
  pub json: bool,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn install_and_update_share_one_explicit_source_and_target_surface() {
    for command in ["install", "update"] {
      assert!(
        Arguments::try_parse_from([
          "ctlup",
          command,
          "--host",
          "work",
          "--channel",
          "stable",
          "--json"
        ])
        .is_ok()
      );
      assert!(
        Arguments::try_parse_from(["ctlup", command, "--channel", "dev", "--from", "/build"])
          .is_err()
      );
      assert!(Arguments::try_parse_from(["ctlup", command, "--package", "ctl-agent"]).is_err());
      assert!(Arguments::try_parse_from(["ctlup", command, "--selected"]).is_err());
    }
  }

  #[test]
  fn remote_inspection_and_rollback_use_the_same_saved_host_options() {
    for command in ["status", "rollback"] {
      let args = Arguments::try_parse_from(["ctlup", "--host", "work", command, "--json"]).unwrap();
      assert_eq!(args.host.as_deref(), Some("work"));
    }
    assert!(
      Arguments::try_parse_from([
        "ctlup",
        "--host",
        "work",
        "--destination",
        "server",
        "status"
      ])
      .is_err()
    );
  }
}
