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
  /// Build and install one complete native development build from a checkout.
  Dev(DevelopmentArguments),
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
#[expect(
  clippy::struct_excessive_bools,
  reason = "clap parses independent command-line flags"
)]
pub(super) struct UpdateArguments {
  /// Saved host names or IDs; repeat or separate with commas.
  #[arg(long, value_delimiter = ',', num_args = 1..)]
  pub hosts: Vec<String>,
  /// Include this computer; the default when no host is supplied.
  #[arg(long)]
  pub local: bool,
  /// Publication channel; remembered after a successful update.
  #[arg(long, value_enum, conflicts_with_all = ["from", "working_tree"])]
  pub channel: Option<crate::components::Channel>,
  /// Complete build directory or archive for an explicit offline installation.
  #[arg(long, conflicts_with_all = ["channel", "working_tree"])]
  pub from: Option<PathBuf>,
  /// Cross-build a Linux x64 or ARM64 bundle from this checkout.
  #[arg(long)]
  pub working_tree: bool,
  /// Inspect and snapshot native binaries from --from.
  #[arg(long, requires = "from")]
  pub local_build: bool,
  /// Complete signed ctld.app package and receipt for a native macOS build.
  #[arg(long, requires = "local_build")]
  pub ctld_package: Option<PathBuf>,
  #[arg(long)]
  pub json: bool,
}

#[derive(Debug, Args)]
pub(super) struct DevelopmentArguments {
  /// Source checkout; defaults to the current directory.
  #[arg(long)]
  pub workspace: Option<PathBuf>,
  #[arg(long, conflicts_with_all = ["profile", "cargo_args_json"])]
  pub release: bool,
  #[arg(long, conflicts_with = "cargo_args_json")]
  pub profile: Option<String>,
  #[arg(long, conflicts_with = "cargo_args_json")]
  pub target_dir: Option<PathBuf>,
  /// Native Rust target; foreign-target development installations are rejected.
  #[arg(long, conflicts_with = "cargo_args_json")]
  pub target: Option<String>,
  /// Original Cargo invocation supplied by the desktop's build adapter.
  #[arg(long, hide = true)]
  pub cargo_args_json: Option<String>,
  /// Keep the prepared installation leased until the launcher's input closes.
  #[arg(long, hide = true, requires = "json")]
  pub lease: bool,
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
      assert!(Arguments::try_parse_from(["ctlup", command, "--working-tree"]).is_ok());
      assert!(
        Arguments::try_parse_from(["ctlup", command, "--working-tree", "--channel", "dev"])
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

  #[test]
  fn native_development_builds_accept_one_cargo_configuration() {
    let args = Arguments::try_parse_from([
      "ctlup",
      "dev",
      "--workspace",
      "/checkout",
      "--profile",
      "custom",
      "--target-dir",
      "/build-output",
      "--json",
    ])
    .unwrap();
    assert!(matches!(
      args.command,
      Command::Dev(DevelopmentArguments {
        release: false,
        profile: Some(_),
        ..
      })
    ));
    for flags in [
      vec!["--release", "--profile", "custom"],
      vec!["--release", "--cargo-args-json", "[]"],
      vec!["--target-dir", "/output", "--cargo-args-json", "[]"],
    ] {
      assert!(Arguments::try_parse_from([vec!["ctlup", "dev"], flags].concat()).is_err());
    }
    assert!(Arguments::try_parse_from(["ctlup", "dev", "--lease"]).is_err());
    assert!(Arguments::try_parse_from(["ctlup", "dev", "--lease", "--json"]).is_ok());
  }
}
