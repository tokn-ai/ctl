//! Open validated web links with the system's default browser.

use std::future::Future;
use std::process::Stdio;
use std::time::Duration;

use crate::error::{CommandErrorDto, CommandResult};

#[tauri::command]
pub async fn open_external_url(url: String) -> CommandResult<()> {
  open(&url).await
}

pub(crate) async fn open(url: &str) -> CommandResult<()> {
  open_with(url, launch).await
}

async fn open_with<F, S>(url: &str, launcher: F) -> CommandResult<()>
where
  F: FnOnce(tauri::Url) -> S,
  S: Future<Output = CommandResult<()>>,
{
  let url = validate_url(url)?;
  launcher(url).await
}

async fn launch(url: tauri::Url) -> CommandResult<()> {
  #[cfg(unix)]
  let mut command = browser_command(url.as_str());
  #[cfg(windows)]
  let mut command = browser_command(url.as_str())?;
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

fn validate_url(value: &str) -> CommandResult<tauri::Url> {
  if value.trim() != value || value.chars().any(char::is_control) {
    return Err(invalid_url_error());
  }
  let (scheme, remainder) = value.split_once("://").ok_or_else(invalid_url_error)?;
  if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
    return Err(invalid_url_error());
  }
  // URL parsers repair missing authorities and backslashes. Require an
  // explicit authority so displayed text cannot silently change its meaning.
  let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
  if authority.is_empty() || authority.contains('\\') {
    return Err(invalid_url_error());
  }
  let url = tauri::Url::parse(value).map_err(|_| invalid_url_error())?;
  if url.host_str().is_none() {
    return Err(invalid_url_error());
  }
  Ok(url)
}

#[cfg(unix)]
fn browser_command(url: &str) -> tokio::process::Command {
  #[cfg(target_os = "macos")]
  {
    let mut command = tokio::process::Command::new("/usr/bin/open");
    command.arg("--").arg(url);
    command
  }
  #[cfg(all(unix, not(target_os = "macos")))]
  {
    let mut command = tokio::process::Command::new("xdg-open");
    command.arg(url);
    command
  }
}

#[cfg(windows)]
fn browser_command(url: &str) -> CommandResult<tokio::process::Command> {
  let system_directory =
    std::path::PathBuf::from(std::env::var_os("SystemRoot").ok_or_else(browser_error)?)
      .join("System32");
  // The URL remains one argument and never enters a shell command string.
  let mut command = tokio::process::Command::new(system_directory.join("rundll32.exe"));
  command
    .current_dir(system_directory)
    .args(["url.dll,FileProtocolHandler", url]);
  Ok(command)
}

fn invalid_url_error() -> CommandErrorDto {
  CommandErrorDto::new(
    "invalid_external_url",
    "Only valid HTTP or HTTPS links can be opened.",
  )
}

fn browser_error() -> CommandErrorDto {
  CommandErrorDto::new(
    "browser_open_failed",
    "Could not open the link in your browser.",
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn accepts_web_urls_and_preserves_their_content() {
    for value in [
      "https://example.com/docs?q=terminal&lang=en#links",
      "http://localhost:3000/path",
      "http://127.0.0.1:8080/",
      "http://[::1]:8080/",
      "https://example.com/a%20path?q=%26%23",
      "https://user:password@example.com/",
    ] {
      assert_eq!(validate_url(value).unwrap().as_str(), value);
    }
    assert_eq!(
      validate_url("HTTPS://EXAMPLE.COM/path").unwrap().as_str(),
      "https://example.com/path"
    );
  }

  #[tokio::test]
  async fn rejects_non_web_or_ambiguous_urls_before_launch() {
    for value in [
      "",
      "example.com",
      "//example.com",
      "https:example.com",
      "https:///example.com",
      "https://",
      "https://?example.com",
      "https://#example.com",
      "https://example.com:invalid/",
      "https://example.com\\@attacker.test/",
      "file:///tmp/file",
      "javascript:alert(1)",
      "mailto:user@example.com",
      "ftp://example.com/file",
      " https://example.com",
      "https://example.com ",
      "https://example.com\n/path",
      "https://example.com/\u{7f}",
    ] {
      assert_eq!(
        open_with(value, |_| async {
          panic!("invalid URL must not reach the launcher")
        })
        .await
        .unwrap_err()
        .code,
        "invalid_external_url",
        "{value:?}"
      );
    }
  }

  #[test]
  fn passes_url_as_a_single_argument_without_a_shell() {
    let url = "https://example.com/?q=one&next=$(command);two#fragment";
    #[cfg(unix)]
    let command = browser_command(url);
    #[cfg(windows)]
    let command = browser_command(url).unwrap();
    let command = command.as_std();
    let arguments: Vec<_> = command.get_args().collect();
    #[cfg(target_os = "macos")]
    {
      assert_eq!(command.get_program(), "/usr/bin/open");
      assert_eq!(arguments, ["--", url]);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
      assert_eq!(command.get_program(), "xdg-open");
      assert_eq!(arguments, [url]);
    }
    #[cfg(windows)]
    {
      let system_directory =
        std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
      assert_eq!(command.get_program(), system_directory.join("rundll32.exe"));
      assert_eq!(command.get_current_dir(), Some(system_directory.as_path()));
      assert_eq!(arguments, ["url.dll,FileProtocolHandler", url]);
    }
  }
}
