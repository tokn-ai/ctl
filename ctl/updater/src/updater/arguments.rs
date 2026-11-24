use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use ctl_client::cli::Channel;
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
  name = "ctlup",
  version,
  about = "Install and update ctl without restarting running services"
)]
pub(super) struct Arguments {
  #[command(subcommand)]
  pub command: Command,
}

#[derive(Debug, Default, Args)]
pub(super) struct DestinationArguments {
  /// Select a saved ctl host by name or ID. Repeat for multiple hosts.
  #[arg(long, short = 'H')]
  pub host: Vec<String>,
  /// Use an SSH URI, address or OpenSSH alias without saving a host.
  #[arg(long, conflicts_with_all = ["host", "method"])]
  pub ssh: Option<String>,
  /// Select one saved host's connection method.
  #[arg(long, requires = "host")]
  pub method: Option<String>,
  /// Override the SSH host platform for this invocation.
  #[arg(long, value_enum)]
  pub remote_platform: Option<crate::RemotePlatform>,
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
  #[command(long_flag = "component-info", hide = true)]
  ComponentInfo,
  /// Select a source and install the runtime or desktop app.
  Install(InstallArguments),
  /// Update the selected package from its tracked channel.
  Update(UpdateArguments),
  /// Install, update, or launch the standalone ctream tools.
  Ctream {
    #[command(subcommand)]
    command: CtreamCommand,
  },
  /// Fetch, inspect or remove retained runtime bundles by target.
  Bundle {
    #[command(subcommand)]
    command: BundleCommand,
  },
  /// Build and install from the current checkout.
  Dev(DevelopmentArguments),
  /// Sign and notarize downloaded macOS desktop build artifacts locally.
  Sign(SignArguments),
  /// Show installed builds, their sources and missing payloads.
  Status(StatusArguments),
  /// Browse available releases and pull request builds.
  Release(ReleaseArguments),
  /// Remove unused builds and cached files.
  Prune {
    /// Forget recorded builds whose payloads are missing.
    #[arg(long)]
    forget_missing: bool,
    #[arg(long)]
    json: bool,
  },
  #[command(hide = true)]
  InspectRuntime {
    #[arg(long)]
    json: bool,
  },
  #[command(hide = true)]
  RuntimeSource,
  #[command(hide = true)]
  InstallManager,
  #[command(hide = true)]
  ExportManager {
    #[arg(long)]
    sha256: String,
  },
  #[command(hide = true)]
  InstallArchive {
    #[arg(long, hide = true)]
    origin_hex: Option<String>,
  },
  #[command(hide = true)]
  InstallPublication(PublicationArguments),
  #[command(hide = true)]
  ActivateInstalled {
    bundle_id: String,
    #[arg(long, hide = true)]
    origin_hex: Option<String>,
  },
  #[command(hide = true)]
  CatalogJson,
  #[command(hide = true)]
  PrepareInstall,
  #[command(hide = true)]
  InstallationLayoutVersion,
  #[command(hide = true)]
  UpdaterDigest,
}

#[derive(Debug, Args)]
pub(super) struct PublicationArguments {
  #[arg(long)]
  pub origin_hex: String,
  #[arg(long)]
  pub archive_url: String,
  #[arg(long)]
  pub target: String,
  #[arg(long)]
  pub manifest_bytes: usize,
  #[arg(long)]
  pub archive_stdin: bool,
}

#[derive(Debug, Subcommand)]
pub(super) enum BundleCommand {
  /// List retained target bundles.
  List {
    #[command(flatten)]
    destination: DestinationArguments,
    /// Filter installed provenance; stable and nightly also support --available.
    #[arg(long)]
    channel: Option<String>,
    /// Include targets in the current publication.
    #[arg(long)]
    available: bool,
    #[arg(long)]
    json: bool,
  },
  /// Retain verified target bundles for offline use.
  Fetch {
    #[arg(long = "target", required = true, value_name = "TRIPLE")]
    targets: Vec<String>,
    #[arg(long, value_enum, conflicts_with_all = ["pr", "from", "release"])]
    channel: Option<Channel>,
    #[arg(long, value_name = "PUBLICATION_ID", conflicts_with_all = ["pr", "from"])]
    release: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..), conflicts_with = "from")]
    pr: Option<u64>,
    /// An explicit offline bundle set or complete build directory.
    #[arg(long)]
    from: Option<PathBuf>,
    #[arg(long)]
    json: bool,
  },
  /// Remove target registrations while preserving active and running builds.
  Remove {
    #[arg(long = "target", required = true, value_name = "TRIPLE")]
    targets: Vec<String>,
    /// Installed provenance, such as stable, nightly, local, provided, or pr-123.
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    json: bool,
  },
}

#[derive(Debug, Subcommand)]
pub(super) enum CtreamCommand {
  /// Install ctream and select the new build.
  Install(CtreamInstallArguments),
  /// Update the selected ctream installation.
  Update(CtreamInstallArguments),
  /// Show the selected local ctream build and executable paths.
  Status {
    #[arg(long)]
    json: bool,
  },
  /// Open the installed ctream preview.
  Run {
    /// Arguments for ctream-preview, after --.
    #[arg(last = true)]
    args: Vec<OsString>,
  },
  /// Run the installed ctream CLI.
  Cli {
    /// Arguments for ctream, after --.
    #[arg(last = true)]
    args: Vec<OsString>,
  },
}

#[derive(Debug, Args)]
pub(super) struct CtreamInstallArguments {
  /// A ctream preview ZIP from the bundle workflow, for offline installation.
  #[arg(long, conflicts_with = "run")]
  pub from: Option<PathBuf>,
  /// Use a specific successful ctream preview workflow run from this repository.
  #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
  pub run: Option<u64>,
  #[arg(long)]
  pub json: bool,
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
  Show {
    /// Publication ID from `ctlup release list`; defaults to the current release.
    publication_id: Option<String>,
    /// Published channel; defaults to the configured channel.
    #[arg(long, value_enum)]
    channel: Option<Channel>,
    /// Pull request number.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..), conflicts_with_all = ["channel", "publication_id"])]
    pr: Option<u64>,
  },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum ReleaseListChannel {
  Dev,
  Nightly,
  Stable,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(super) enum PackageSelection {
  #[default]
  Runtime,
  App,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("source").args(["channel", "release", "pr", "from"])))]
pub(super) struct InstallArguments {
  #[command(flatten)]
  pub destination: DestinationArguments,
  #[arg(value_enum, default_value = "runtime")]
  pub package: PackageSelection,
  /// Track the selected publication channel.
  #[arg(long, value_enum)]
  pub channel: Option<Channel>,
  /// Pin an immutable publication ID from 'ctlup release list'.
  #[arg(long, value_name = "PUBLICATION_ID")]
  pub release: Option<String>,
  /// Install a verified local bundle or app archive.
  #[arg(long, value_name = "PATH")]
  pub from: Option<PathBuf>,
  /// Pin the current build of a pull request.
  #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
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
pub(super) struct UpdateArguments {
  #[command(flatten)]
  pub destination: DestinationArguments,
  #[arg(value_enum, default_value = "runtime")]
  pub package: PackageSelection,
  /// Check the recorded source without installing anything.
  #[arg(long)]
  pub check: bool,
  #[arg(long)]
  pub json: bool,
}

#[derive(Debug, Args)]
pub(super) struct StatusArguments {
  #[command(flatten)]
  pub destination: DestinationArguments,
  #[arg(value_enum, default_value = "runtime")]
  pub package: PackageSelection,
  #[arg(long)]
  pub json: bool,
}

#[derive(Debug, Args)]
pub(super) struct SignArguments {
  /// Tagged release version. Defaults to the active stable desktop version.
  #[arg(long)]
  pub version: Option<String>,
  /// macOS Rust target. Defaults to the current architecture.
  #[arg(long, value_parser = ["aarch64-apple-darwin", "x86_64-apple-darwin"], value_name = "TRIPLE")]
  pub target: Option<String>,
  /// Output the result as JSON.
  #[arg(long)]
  pub json: bool,
}

#[derive(Debug, Args)]
#[expect(
  clippy::struct_excessive_bools,
  reason = "clap parses independent command-line flags"
)]
pub(super) struct DevelopmentArguments {
  #[command(flatten)]
  pub destination: DestinationArguments,
  /// Path to a ctl checkout root. Defaults to the current directory.
  #[arg(long)]
  pub workspace: Option<PathBuf>,
  /// Build and install the native desktop app in the development profile.
  #[arg(long, conflicts_with_all = ["release", "profile", "target_dir", "target", "cargo_args_json", "lease"])]
  pub app: bool,
  /// Build and activate ctream from this checkout.
  #[arg(long, conflicts_with_all = ["app", "host", "ssh", "cargo_args_json", "lease"])]
  pub ctream: bool,
  /// Additional ctream agent Rust target to build. Repeat for multiple targets.
  #[arg(long = "agent-target", requires = "ctream", value_name = "TRIPLE")]
  pub ctream_agent_targets: Vec<String>,
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
  fn install_and_update_distinguish_source_selection_from_tracking() {
    let args = Arguments::try_parse_from([
      "ctlup",
      "install",
      "--ssh",
      "ssh://operator@windows.example:2022",
      "--remote-platform",
      "windows",
      "--channel",
      "stable",
    ])
    .unwrap();
    assert!(matches!(
      args.command,
      Command::Install(InstallArguments {
        package: PackageSelection::Runtime,
        channel: Some(Channel::Stable),
        destination: DestinationArguments {
          ssh: Some(ssh),
          remote_platform: Some(crate::RemotePlatform::Windows),
          ..
        },
        ..
      }) if ssh == "ssh://operator@windows.example:2022"
    ));

    let args = Arguments::try_parse_from(["ctlup", "update", "app", "--check", "--json"]).unwrap();
    assert!(matches!(
      args.command,
      Command::Update(UpdateArguments {
        package: PackageSelection::App,
        check: true,
        json: true,
        ..
      })
    ));
    let args =
      Arguments::try_parse_from(["ctlup", "install", "app", "--release", "publication-1"]).unwrap();
    assert!(matches!(args.command, Command::Install(InstallArguments {
      package: PackageSelection::App, release: Some(id), ..
    }) if id == "publication-1"));

    for pair in [
      ["--channel", "nightly", "--from", "build"],
      ["--channel", "nightly", "--release", "release"],
      ["--pr", "42", "--release", "release"],
      ["--pr", "42", "--from", "build"],
    ] {
      assert!(Arguments::try_parse_from([&["ctlup", "install"][..], &pair].concat()).is_err());
    }
    assert!(Arguments::try_parse_from(["ctlup", "update", "--channel", "stable"]).is_err());
  }

  #[test]
  fn targets_use_one_address_or_repeated_saved_hosts() {
    let args =
      Arguments::try_parse_from(["ctlup", "update", "--host", "work", "--host", "lab"]).unwrap();
    assert!(matches!(args.command, Command::Update(UpdateArguments {
      destination: DestinationArguments { host, .. }, ..
    }) if host == ["work", "lab"]));
    assert!(Arguments::try_parse_from(["ctlup", "--host", "work", "update"]).is_err());
    assert!(
      Arguments::try_parse_from(["ctlup", "install", "--ssh", "work", "--host", "lab",]).is_err()
    );
    assert!(Arguments::try_parse_from(["ctlup", "update", "--method", "work"]).is_err());
    for old in ["--hosts", "--destination", "--local"] {
      assert!(Arguments::try_parse_from(["ctlup", "update", old]).is_err());
    }
  }

  #[test]
  fn inspection_names_and_private_ssh_commands_parse() {
    for command in [
      vec!["status", "--json"],
      vec!["status", "app", "--json"],
      vec!["inspect-runtime", "--json"],
      vec!["runtime-source"],
      vec!["catalog-json"],
      vec!["install-archive"],
      vec!["install-manager"],
      vec!["export-manager", "--sha256", "digest"],
      vec!["prepare-install"],
      vec!["activate-installed", "id"],
      vec!["release", "show", "publication-1"],
      vec!["release", "list", "--channel", "nightly"],
      vec!["bundle", "fetch", "--target", "aarch64-unknown-linux-musl"],
      vec!["bundle", "list"],
      vec!["prune", "--forget-missing"],
    ] {
      assert!(Arguments::try_parse_from([vec!["ctlup"], command].concat()).is_ok());
    }

    for obsolete in [
      vec!["show"],
      vec!["check"],
      vec!["app", "install"],
      vec!["component", "list"],
      vec!["release", "view"],
      vec!["release", "build", "--pr", "42"],
      vec!["migrate"],
      vec!["rollback"],
    ] {
      assert!(Arguments::try_parse_from([vec!["ctlup"], obsolete].concat()).is_err());
    }
  }

  #[test]
  fn source_builds_use_dev() {
    for command in [
      vec!["dev"],
      vec!["dev", "--app"],
      vec!["dev", "--ssh", "lab"],
      vec!["dev", "--host", "lab", "--host", "work"],
      vec!["dev", "--lease", "--json"],
      vec!["dev", "--ctream", "--agent-target", "x86_64-pc-windows-gnu"],
      vec!["ctream", "install", "--run", "42"],
      vec!["ctream", "status"],
      vec!["ctream", "run", "--", "--title", "desktop"],
    ] {
      assert!(Arguments::try_parse_from([vec!["ctlup"], command].concat()).is_ok());
    }
    for invalid in [
      vec!["dev", "--renew-signing"],
      vec!["dev", "sign"],
      vec!["dev", "--app", "--target", "x"],
      vec!["dev", "--lease"],
      vec!["dev", "--ctream", "--ssh", "lab"],
      vec!["install", "--working-tree"],
    ] {
      assert!(Arguments::try_parse_from([vec!["ctlup"], invalid].concat()).is_err());
    }
  }

  #[test]
  fn signing_is_a_top_level_command_for_downloaded_macos_builds() {
    let args = Arguments::try_parse_from(["ctlup", "sign"]).unwrap();
    assert!(matches!(
      args.command,
      Command::Sign(SignArguments {
        version: None,
        target: None,
        json: false,
      })
    ));
    let args = Arguments::try_parse_from(["ctlup", "sign", "--version", "v0.1.5"]).unwrap();
    assert!(
      matches!(args.command, Command::Sign(SignArguments { version: Some(version), .. }) if version == "v0.1.5")
    );
    for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
      let args =
        Arguments::try_parse_from(["ctlup", "sign", "--target", target, "--json"]).unwrap();
      assert!(
        matches!(args.command, Command::Sign(SignArguments { target: Some(selected), json: true, .. }) if selected == target)
      );
    }
    assert!(
      Arguments::try_parse_from(["ctlup", "sign", "--target", "x86_64-unknown-linux-gnu"]).is_err()
    );
  }
}
