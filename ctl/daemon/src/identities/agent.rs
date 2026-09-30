//! Isolated local OpenSSH agent and trusted askpass channel.

use super::{IdentityError, IdentitySnapshot, VerifiedIdentity, files};
use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, Command};
use zeroize::Zeroizing;

const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PASSPHRASE_BYTES: usize = 16 * 1024;

struct VerifiedPublicKey {
  key_type: String,
  fingerprint: String,
  public_key: String,
}

pub struct LocalAgent {
  child: Child,
  directory: PathBuf,
  socket: PathBuf,
  helper_program: PathBuf,
}

impl Drop for LocalAgent {
  fn drop(&mut self) {
    let _ = self.child.start_kill();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

#[cfg(unix)]
impl LocalAgent {
  /// Start an isolated agent without loading or modifying the user's agent.
  ///
  /// # Errors
  /// Returns a sanitized error when system OpenSSH cannot start.
  pub async fn start() -> Result<Self, IdentityError> {
    Self::start_with_program(&std::env::current_exe().map_err(|_| IdentityError::UnlockFailed)?)
      .await
  }

  /// Start using the signed ctld executable selected by the caller.
  ///
  /// # Errors
  /// Returns a sanitized failure if the helper or local agent cannot start.
  pub async fn start_with_program(helper_program: &Path) -> Result<Self, IdentityError> {
    use std::os::unix::fs::DirBuilderExt as _;
    // macOS's per-user temporary directory can exceed Unix socket path limits.
    // Atomic 0700 creation below protects this unpredictable shared-temp path.
    let directory = Path::new("/tmp").join(format!("ctld-key-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&directory)
      .map_err(|_| IdentityError::UnlockFailed)?;
    let socket = directory.join("agent");
    let child = Command::new("/usr/bin/ssh-agent")
      .arg("-a")
      .arg(&socket)
      .arg(helper_program)
      .arg("--identity-agent-lifetime")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn();
    let Ok(child) = child else {
      let _ = std::fs::remove_dir_all(&directory);
      return Err(IdentityError::UnlockFailed);
    };
    let mut agent = Self {
      child,
      directory,
      socket,
      helper_program: helper_program.to_path_buf(),
    };
    let ready = tokio::time::timeout(Duration::from_secs(3), async {
      loop {
        if tokio::net::UnixStream::connect(&agent.socket).await.is_ok() {
          return Ok(());
        }
        if agent
          .child
          .try_wait()
          .map_err(|_| IdentityError::UnlockFailed)?
          .is_some()
        {
          return Err(IdentityError::UnlockFailed);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .map_err(|_| IdentityError::UnlockFailed)?;
    ready?;
    Ok(agent)
  }

  /// Locally decrypt an immutable snapshot; no SSH connection is attempted.
  ///
  /// # Errors
  /// Returns a sanitized failure for a wrong passphrase, unsupported key, or timeout.
  pub async fn add_identity(
    &mut self,
    snapshot: &IdentitySnapshot,
    passphrase: Zeroizing<String>,
  ) -> Result<VerifiedIdentity, IdentityError> {
    super::ensure_current(snapshot)?;
    let expected = if snapshot.fingerprint.is_none() {
      // PEM envelopes do not expose their public key. A separate empty agent
      // prevents an existing identity in this agent from being misidentified.
      let mut verifier = Self::start_with_program(&self.helper_program).await?;
      Some(
        verifier
          .add_with_program(snapshot, passphrase.clone(), None)
          .await?
          .fingerprint,
      )
    } else {
      snapshot.fingerprint.clone()
    };
    self
      .add_with_program(snapshot, passphrase, expected.as_deref())
      .await
  }

  async fn add_with_program(
    &mut self,
    snapshot: &IdentitySnapshot,
    passphrase: Zeroizing<String>,
    expected_fingerprint: Option<&str>,
  ) -> Result<VerifiedIdentity, IdentityError> {
    if passphrase.len() > MAX_PASSPHRASE_BYTES || passphrase.contains(['\0', '\r', '\n']) {
      return Err(IdentityError::InvalidRequest);
    }
    let askpass_socket = self
      .directory
      .join(format!("ask-{}", uuid::Uuid::new_v4().simple()));
    let listener =
      tokio::net::UnixListener::bind(&askpass_socket).map_err(|_| IdentityError::UnlockFailed)?;
    let token = uuid::Uuid::new_v4().to_string();
    let passphrase_digest = super::passphrase_digest(&passphrase);
    let broker_token = token.clone();
    let broker = AbortTask(tokio::spawn(async move {
      for _ in 0..3 {
        let (mut stream, _) = listener
          .accept()
          .await
          .map_err(|_| IdentityError::UnlockFailed)?;
        let mut supplied = Vec::new();
        (&mut stream)
          .take(128)
          .read_to_end(&mut supplied)
          .await
          .map_err(|_| IdentityError::UnlockFailed)?;
        if supplied != broker_token.as_bytes() {
          continue;
        }
        stream
          .write_all(passphrase.as_bytes())
          .await
          .map_err(|_| IdentityError::UnlockFailed)?;
        stream
          .write_all(b"\n")
          .await
          .map_err(|_| IdentityError::UnlockFailed)?;
        stream
          .shutdown()
          .await
          .map_err(|_| IdentityError::UnlockFailed)?;
      }
      Ok::<(), IdentityError>(())
    }));
    let mut command = Command::new("/usr/bin/ssh-add");
    command
      .args(["-k", "-"])
      .env("SSH_AUTH_SOCK", &self.socket)
      .env("SSH_ASKPASS", &self.helper_program)
      .env("SSH_ASKPASS_REQUIRE", "force")
      .env("DISPLAY", "ctld-local-identity")
      .env("CTLD_IDENTITY_ASKPASS", "1")
      .env("CTLD_IDENTITY_ASKPASS_SOCKET", &askpass_socket)
      .env("CTLD_IDENTITY_ASKPASS_TOKEN", &token)
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_ASKPASS_TOKEN")
      .env_remove("SSH_ASKPASS_PROMPT")
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true);
    let result = tokio::time::timeout(VERIFY_TIMEOUT, async {
      let mut child = command.spawn().map_err(|_| IdentityError::UnlockFailed)?;
      let mut stdin = child.stdin.take().ok_or(IdentityError::UnlockFailed)?;
      stdin
        .write_all(&snapshot.bytes)
        .await
        .map_err(|_| IdentityError::UnlockFailed)?;
      drop(stdin);
      let status = child
        .wait()
        .await
        .map_err(|_| IdentityError::UnlockFailed)?;
      if !status.success() {
        return Err(IdentityError::UnlockFailed);
      }
      self.verified_key(expected_fingerprint).await
    })
    .await
    .map_err(|_| IdentityError::UnlockFailed)
    .and_then(std::convert::identity);
    drop(broker);
    let _ = std::fs::remove_file(askpass_socket);
    result.map(|public| VerifiedIdentity {
      key_type: public.key_type,
      fingerprint: public.fingerprint,
      public_key: public.public_key,
      identity_id: snapshot.identity_id.clone(),
      file_version: snapshot.file_version.clone(),
      passphrase_digest,
    })
  }

  async fn verified_key(
    &self,
    expected_fingerprint: Option<&str>,
  ) -> Result<VerifiedPublicKey, IdentityError> {
    let mut child = Command::new("/usr/bin/ssh-add")
      .arg("-L")
      .env("SSH_AUTH_SOCK", &self.socket)
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .map_err(|_| IdentityError::UnlockFailed)?;
    let mut output = Vec::new();
    child
      .stdout
      .take()
      .ok_or(IdentityError::UnlockFailed)?
      .take(1024 * 1024 + 1)
      .read_to_end(&mut output)
      .await
      .map_err(|_| IdentityError::UnlockFailed)?;
    if output.len() > 1024 * 1024
      || !child
        .wait()
        .await
        .map_err(|_| IdentityError::UnlockFailed)?
        .success()
    {
      return Err(IdentityError::UnlockFailed);
    }
    for line in std::str::from_utf8(&output)
      .map_err(|_| IdentityError::UnlockFailed)?
      .lines()
      .rev()
    {
      let mut fields = line.split_whitespace();
      let Some(key_type) = fields.next() else {
        continue;
      };
      let Some(encoded) = fields.next() else {
        continue;
      };
      let blob = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| IdentityError::UnlockFailed)?;
      let (actual_type, fingerprint) = files::public_metadata(&blob)?;
      if actual_type != key_type
        || expected_fingerprint.is_some_and(|expected| expected != fingerprint)
      {
        continue;
      }
      return Ok(VerifiedPublicKey {
        key_type: actual_type,
        fingerprint,
        public_key: format!("{key_type} {encoded}"),
      });
    }
    Err(IdentityError::UnlockFailed)
  }
}

impl LocalAgent {
  #[must_use]
  pub fn socket_path(&self) -> &Path {
    &self.socket
  }
}

struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
  fn drop(&mut self) {
    self.0.abort();
  }
}

/// Service only the local broker created by an identity verification subprocess.
#[must_use]
pub fn askpass_exit_code() -> Option<i32> {
  if std::env::var("CTLD_IDENTITY_ASKPASS").ok().as_deref() != Some("1") {
    return None;
  }
  #[cfg(unix)]
  return Some(i32::from(run_askpass().is_err()));
  #[cfg(not(unix))]
  Some(1)
}

#[cfg(unix)]
fn run_askpass() -> Result<(), IdentityError> {
  use std::io::{Read as _, Write as _};
  use std::os::unix::net::UnixStream;
  let path = std::env::var_os("CTLD_IDENTITY_ASKPASS_SOCKET").ok_or(IdentityError::UnlockFailed)?;
  let token =
    std::env::var("CTLD_IDENTITY_ASKPASS_TOKEN").map_err(|_| IdentityError::UnlockFailed)?;
  if token.len() > 128 {
    return Err(IdentityError::UnlockFailed);
  }
  let mut stream = UnixStream::connect(path).map_err(|_| IdentityError::UnlockFailed)?;
  stream
    .set_read_timeout(Some(VERIFY_TIMEOUT))
    .map_err(|_| IdentityError::UnlockFailed)?;
  stream
    .set_write_timeout(Some(VERIFY_TIMEOUT))
    .map_err(|_| IdentityError::UnlockFailed)?;
  stream
    .write_all(token.as_bytes())
    .map_err(|_| IdentityError::UnlockFailed)?;
  stream
    .shutdown(std::net::Shutdown::Write)
    .map_err(|_| IdentityError::UnlockFailed)?;
  let mut bytes = Zeroizing::new(Vec::new());
  stream
    .take((MAX_PASSPHRASE_BYTES + 2) as u64)
    .read_to_end(&mut bytes)
    .map_err(|_| IdentityError::UnlockFailed)?;
  if bytes.len() > MAX_PASSPHRASE_BYTES + 1 || bytes.last() != Some(&b'\n') {
    return Err(IdentityError::UnlockFailed);
  }
  std::io::stdout()
    .lock()
    .write_all(&bytes)
    .map_err(|_| IdentityError::UnlockFailed)
}

/// Keep OpenSSH's command-mode agent alive while the owner's pipe is open.
/// A killed helper closes that pipe too, so the agent cannot become orphaned.
///
/// # Errors
/// Returns a read error from the private lifetime pipe.
pub fn run_lifetime() -> std::io::Result<()> {
  std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink())?;
  Ok(())
}
