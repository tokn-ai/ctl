use super::arguments::{ReleaseArguments, ReleaseCommand, ReleaseListChannel};
use super::{print_targets, selected_channel};
use crate::components::Channel;
use ctl_client::remote_bundle::pr::{self, PullRequestBuildRequestStatus, PullRequestBuildStatus};
use ctl_client::remote_bundle::published;
use serde::Serialize;
use std::io;
use std::path::Path;

pub(super) async fn run(home: &Path, options: ReleaseArguments) -> io::Result<()> {
  match options.command {
    ReleaseCommand::List { channel } => {
      let channel = match channel {
        Some(ReleaseListChannel::Nightly) => Some(Channel::Nightly),
        Some(ReleaseListChannel::Stable) => Some(Channel::Stable),
        None => None,
        Some(ReleaseListChannel::Dev) => return list_prs(options.json).await,
      };
      let channel = selected_channel(home, channel)?;
      let releases = published::list_releases(channel)
        .await
        .map_err(io::Error::other)?;
      if options.json {
        print_json(&releases)?;
      } else if releases.is_empty() {
        println!("No {} releases are published.", channel.name());
      } else {
        for release in releases {
          println!(
            "{}{} (built {})",
            release.publication_id,
            if release.current { " (current)" } else { "" },
            release.built_at.as_deref().unwrap_or("unknown")
          );
        }
      }
      Ok(())
    }
    ReleaseCommand::View {
      pr: Some(number), ..
    } => view_pr(number, options.json).await,
    ReleaseCommand::View {
      channel,
      publication_id,
      pr: None,
    } => {
      let channel = selected_channel(home, channel)?;
      let details = published::view_release(channel, publication_id.as_deref())
        .await
        .map_err(io::Error::other)?;
      if options.json {
        print_json(&details)?;
      } else {
        let release = details.release;
        println!(
          "{}: {}{}",
          release.channel.name(),
          release.publication_id,
          if release.current { " (current)" } else { "" }
        );
        println!("Version {}", release.bundle_id);
        println!("Revision {}", release.git_revision);
        println!("Built {}", release.built_at.as_deref().unwrap_or("unknown"));
        print_targets(&details.targets);
      }
      Ok(())
    }
    ReleaseCommand::Build { pr: number } => build_pr(number, options.json).await,
  }
}

async fn list_prs(json: bool) -> io::Result<()> {
  let requests = pr::list().await.map_err(io::Error::other)?;
  if json {
    return print_json(&requests);
  }
  if requests.is_empty() {
    println!("No recent pull requests.");
  }
  for request in requests {
    let status = match request.status {
      PullRequestBuildStatus::Built => "built",
      PullRequestBuildStatus::Outdated => "outdated",
      PullRequestBuildStatus::NoBuild => "no build",
    };
    println!(
      "#{} [{}] {}",
      request.number,
      status,
      terminal_title(&request.title)
    );
  }
  Ok(())
}

async fn view_pr(number: u64, json: bool) -> io::Result<()> {
  let request = pr::view(number).await.map_err(io::Error::other)?;
  if json {
    return print_json(&request);
  }
  println!("PR #{} {}", request.number, terminal_title(&request.title));
  println!("{}", request.url);
  println!("Revision {}", request.head_sha);
  if let Some(build) = request.build {
    println!("Build run {} ({})", build.run_id, build.url);
    println!("Built {}", build.created_at);
    println!("Expires {}", build.expires_at);
  } else if request.status == PullRequestBuildStatus::Outdated {
    println!("Outdated build available for an earlier commit.");
  } else {
    println!("No build for this commit.");
  }
  Ok(())
}

async fn build_pr(number: u64, json: bool) -> io::Result<()> {
  let request = pr::trigger_build(number).await.map_err(io::Error::other)?;
  if json {
    return print_json(&request);
  }
  match request.status {
    PullRequestBuildRequestStatus::Queued => println!("Queued a build for PR #{number}."),
    PullRequestBuildRequestStatus::AlreadyBuilt => {
      println!("PR #{number} already has a bundle for its current commit.");
    }
    PullRequestBuildRequestStatus::InProgress => {
      println!("A build for the current commit of PR #{number} is already in progress.");
    }
  }
  if let Some(url) = request.url {
    println!("{url}");
  }
  Ok(())
}

fn terminal_title(title: &str) -> String {
  title
    .chars()
    .map(|character| {
      if character.is_control() {
        ' '
      } else {
        character
      }
    })
    .collect()
}

fn print_json(value: &impl Serialize) -> io::Result<()> {
  println!(
    "{}",
    serde_json::to_string(value).map_err(io::Error::other)?
  );
  Ok(())
}
