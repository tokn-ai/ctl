use clap::{Args, Parser, Subcommand, ValueEnum};
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
  /// Override the SSH host platform for this invocation.
  #[arg(long, global = true, value_enum)]
  pub remote_platform: Option<crate::RemotePlatform>,
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
  /// Browse published releases and manage pull request builds.
  Release(ReleaseArguments),
  /// Remove unused builds, retaining active, previous and running installations.
  Prune {
    #[arg(long)]
    json: bool,
  },
  #[command(hide = true)]
  InstallArchive,
  #[command(hide = true)]
  ActivateInstalled { bundle_id: String },
  #[cfg(windows)]
  #[command(hide = true)]
  UpdaterDigest,
}

#[derive(Debug, Args)]
pub(super) struct ReleaseArguments {
  /// Output the result as JSON.
  #[arg(long, global = true)]
  pub json: bool,
  #[command(subcommand)]
  pub command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
pub(super) enum ReleaseCommand {
  /// List recent releases or pull requests.
  List {
    /// Release channel; defaults to the configured channel. Dev lists pull requests.
    #[arg(long, value_enum)]
    channel: Option<ReleaseListChannel>,
  },
  /// Show metadata for a release or the current build of a pull request.
  View {
    /// Publication ID from `ctlup release list`; defaults to the current release.
    publication_id: Option<String>,
    /// Published channel; defaults to the configured channel.
    #[arg(long, value_enum)]
    channel: Option<crate::components::Channel>,
    /// Pull request number.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..), conflicts_with_all = ["channel", "publication_id"])]
    pr: Option<u64>,
  },
  /// Request a bundle build for the current commit of a pull request.
  Build {
    /// Pull request number.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pr: u64,
  },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum ReleaseListChannel {
  Dev,
  Nightly,
  Stable,
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
  /// Install the current build of a pull request.
  #[arg(long, value_parser = clap::value_parser!(u64).range(1..), conflicts_with_all = ["channel", "from", "working_tree"])]
  pub pr: Option<u64>,
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
  fn release_commands_accept_a_channel_and_an_optional_publication_id() {
    let args =
      Arguments::try_parse_from(["ctlup", "release", "list", "--channel", "stable", "--json"])
        .unwrap();
    assert!(matches!(
      args.command,
      Command::Release(ReleaseArguments {
        command: ReleaseCommand::List {
          channel: Some(ReleaseListChannel::Stable)
        },
        json: true,
      })
    ));

    let args = Arguments::try_parse_from(["ctlup", "release", "view"]).unwrap();
    assert!(matches!(
      args.command,
      Command::Release(ReleaseArguments {
        command: ReleaseCommand::View {
          publication_id: None,
          channel: None,
          pr: None,
        },
        json: false,
      })
    ));

    let args = Arguments::try_parse_from([
      "ctlup",
      "release",
      "view",
      "0.1.2-123-1",
      "--channel",
      "nightly",
    ])
    .unwrap();
    assert!(matches!(
      args.command,
      Command::Release(ReleaseArguments {
        command: ReleaseCommand::View {
          publication_id: Some(id),
          channel: Some(crate::components::Channel::Nightly),
          pr: None,
        },
        ..
      }) if id == "0.1.2-123-1"
    ));
  }

  #[test]
  fn dev_listing_and_pr_view_use_explicit_preview_selectors() {
    let args = Arguments::try_parse_from(["ctlup", "release", "list", "--channel", "dev"]).unwrap();
    assert!(matches!(
      args.command,
      Command::Release(ReleaseArguments {
        command: ReleaseCommand::List {
          channel: Some(ReleaseListChannel::Dev)
        },
        ..
      })
    ));
    let args = Arguments::try_parse_from(["ctlup", "release", "view", "--pr", "155"]).unwrap();
    assert!(matches!(
      args.command,
      Command::Release(ReleaseArguments {
        command: ReleaseCommand::View { pr: Some(155), .. },
        ..
      })
    ));
    for args in [
      &["release", "list", "--pr", "155"][..],
      &["release", "view", "--pr", "155", "--channel", "nightly"],
      &["release", "view", "--pr", "155", "0.1.2-123-1"],
      &["release", "view", "--pr", "0"],
      &["release", "view", "--channel", "dev"],
      &["check", "--channel", "dev"],
    ] {
      assert!(Arguments::try_parse_from([&["ctlup"][..], args].concat()).is_err());
    }
  }

  #[test]
  fn release_build_requires_a_positive_pr_number_and_accepts_json() {
    let args =
      Arguments::try_parse_from(["ctlup", "release", "build", "--pr", "155", "--json"]).unwrap();
    assert!(matches!(
      args.command,
      Command::Release(ReleaseArguments {
        command: ReleaseCommand::Build { pr: 155 },
        json: true,
      })
    ));
    assert!(Arguments::try_parse_from(["ctlup", "release", "build"]).is_err());
    assert!(Arguments::try_parse_from(["ctlup", "release", "build", "--pr", "0"]).is_err());
  }

  #[test]
  fn install_and_update_share_one_explicit_source_and_target_surface() {
    for command in ["install", "update"] {
      for channel in ["nightly", "stable"] {
        assert!(
          Arguments::try_parse_from([
            "ctlup",
            command,
            "--host",
            "work",
            "--channel",
            channel,
            "--json"
          ])
          .is_ok()
        );
      }
      assert!(
        Arguments::try_parse_from(["ctlup", command, "--channel", "nightly", "--from", "/build"])
          .is_err()
      );
      assert!(Arguments::try_parse_from(["ctlup", command, "--working-tree"]).is_ok());
      assert!(
        Arguments::try_parse_from(["ctlup", command, "--working-tree", "--channel", "nightly"])
          .is_err()
      );
      assert!(Arguments::try_parse_from(["ctlup", command, "--package", "ctl-agent"]).is_err());
      assert!(Arguments::try_parse_from(["ctlup", command, "--selected"]).is_err());
    }
  }

  #[test]
  fn update_can_select_a_pr_and_keep_the_target_options() {
    let args = Arguments::try_parse_from([
      "ctlup", "update", "--pr", "155", "--host", "work", "--local", "--json",
    ])
    .unwrap();
    assert_eq!(args.host.as_deref(), Some("work"));
    assert!(matches!(
      args.command,
      Command::Update(UpdateArguments {
        pr: Some(155),
        local: true,
        json: true,
        ..
      })
    ));
    for flags in [
      &["--pr", "155", "--channel", "stable"][..],
      &["--pr", "155", "--from", "/build"],
      &["--pr", "155", "--working-tree"],
      &["--pr", "0"],
      &["--channel", "dev"],
    ] {
      assert!(Arguments::try_parse_from([&["ctlup", "update"][..], flags].concat()).is_err());
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
