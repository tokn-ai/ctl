//! Open only a current, validated sign-in URL from the selected VPN owner.

use std::future::Future;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::time::Duration;

use ctld_ipc::{VpnProvider, VpnSnapshot, VpnState};

use crate::error::{CommandErrorDto, CommandResult};

pub(super) async fn open<F, S>(snapshot: &VpnSnapshot, vpn_id: &str, opener: F) -> CommandResult<()>
where
  F: FnOnce(String) -> S,
  S: Future<Output = CommandResult<()>>,
{
  let status = snapshot
    .connections
    .iter()
    .find(|status| status.vpn_id.as_deref() == Some(vpn_id))
    .ok_or_else(|| {
      CommandErrorDto::new(
        "vpn_sign_in_unavailable",
        "This VPN is no longer waiting for sign-in. Refresh its status.",
      )
    })?;
  if status.provider != VpnProvider::Tailscale || status.state != VpnState::Starting {
    return Err(CommandErrorDto::new(
      "vpn_sign_in_unavailable",
      "This VPN is not waiting for sign-in. Refresh its status.",
    ));
  }
  let url = status
    .auth_url
    .as_deref()
    .filter(|url| ctld_ipc::vpn::is_tailscale_auth_url(url))
    .ok_or_else(|| {
      CommandErrorDto::new(
        "vpn_sign_in_unavailable",
        "A valid Tailscale sign-in link is not available yet. Refresh its status.",
      )
    })?;
  opener(url.to_owned()).await
}

pub(super) async fn open_browser(url: String) -> CommandResult<()> {
  #[cfg(target_os = "macos")]
  let mut command = {
    let mut command = tokio::process::Command::new("/usr/bin/open");
    command.arg("--").arg(url);
    command
  };
  #[cfg(all(unix, not(target_os = "macos")))]
  let mut command = {
    let mut command = tokio::process::Command::new("xdg-open");
    command.arg(url);
    command
  };
  #[cfg(not(unix))]
  {
    let _ = url;
    return Err(browser_error());
  }
  #[cfg(unix)]
  {
    let status = tokio::time::timeout(
      Duration::from_secs(10),
      command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status(),
    )
    .await
    .map_err(|_| browser_error())?
    .map_err(|_| browser_error())?;
    if status.success() {
      Ok(())
    } else {
      Err(browser_error())
    }
  }
}

fn browser_error() -> CommandErrorDto {
  CommandErrorDto::new(
    "vpn_browser_failed",
    "Could not open the browser for VPN sign-in.",
  )
}

#[cfg(test)]
mod tests {
  use ctld_ipc::VpnStatus;

  use super::*;

  fn snapshot(url: &str) -> VpnSnapshot {
    VpnSnapshot {
      connections: vec![VpnStatus {
        vpn_id: Some("tailscale-one".into()),
        provider: VpnProvider::Tailscale,
        state: VpnState::Starting,
        auth_url: Some(url.into()),
        ..VpnStatus::default()
      }],
      ..VpnSnapshot::default()
    }
  }

  #[tokio::test]
  async fn opens_only_the_matching_current_pending_login() {
    let current = snapshot("https://login.tailscale.com/a/testToken123");
    open(&current, "tailscale-one", |url| async move {
      assert_eq!(url, "https://login.tailscale.com/a/testToken123");
      Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
      open(&current, "missing", |_| async {
        panic!("missing VPN must not open browser")
      })
      .await
      .unwrap_err()
      .code,
      "vpn_sign_in_unavailable",
    );
  }

  #[tokio::test]
  async fn rejects_untrusted_urls_and_completed_login_without_launching() {
    for url in [
      "http://login.tailscale.com/a/testToken123",
      "https://login.tailscale.com.attacker.test/a/testToken123",
      "https://login.tailscale.com@attacker.test/a/testToken123",
      "https://login.tailscale.com/a/testToken123?redirect=elsewhere",
      "https://login.tailscale.com/a/testToken123#fragment",
      "https://login.tailscale.com/a/../admin",
      "file:///private/settings",
    ] {
      assert!(
        open(&snapshot(url), "tailscale-one", |_| async {
          panic!("untrusted URL must not open browser")
        })
        .await
        .is_err()
      );
    }
    let mut current = snapshot("https://login.tailscale.com/a/testToken123");
    current.connections[0].state = VpnState::Connected;
    assert!(
      open(&current, "tailscale-one", |_| async {
        panic!("completed login must not open browser")
      })
      .await
      .is_err()
    );
    current.connections[0].state = VpnState::Starting;
    current.connections[0].provider = VpnProvider::Openconnect;
    assert!(
      open(&current, "tailscale-one", |_| async {
        panic!("non-Tailscale provider must not open browser")
      })
      .await
      .is_err()
    );
  }
}
