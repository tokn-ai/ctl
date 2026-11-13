//! Read-only local and remote file operations with bounded memory and lifetimes.

mod git;

use std::io;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ctl_proto::files::{self, EntryType, FileEntry, Request, Response};
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncSeekExt as _, AsyncWrite};

/// Service admission is independent of the transport's larger stream limit.
/// Keep this guard through response delivery, including blocked consumers.
static SERVICE_REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
static LOCAL_REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// Executes one operation. Dropping the future stops issuing filesystem work
/// and cancels any owned Git process. Already issued OS filesystem calls may
/// finish on Tokio's bounded filesystem worker pool.
///
/// # Errors
/// Returns validation, filesystem, Git, output-limit, or deadline errors.
pub async fn execute(request: Request) -> io::Result<Response> {
  let _permit = LOCAL_REQUESTS.try_acquire().map_err(|_| busy())?;
  request.validate()?;
  tokio::time::timeout(files::OPERATION_TIMEOUT, execute_inner(request))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "File operation timed out"))?
}

async fn execute_inner(request: Request) -> io::Result<Response> {
  let (path, cwd) = request.location();
  let path = resolve(path, cwd)?;
  match request {
    Request::Metadata { .. } => metadata(&path).await,
    Request::ReadFile { offset, length, .. } => read_file(&path, offset, length).await,
    Request::ListDirectory { offset, limit, .. } => list_directory(&path, offset, limit).await,
    Request::GitStatus { .. } => git::status(&path).await,
  }
}

/// Negotiates and serves one operation, monitoring peer closure while work runs.
/// Each caller owns a separate logical stream, so cancellation cannot consume
/// another request's frame or interrupt sibling operations.
///
/// # Errors
/// Returns framing, negotiation, unexpected client data, or transport errors.
pub async fn serve<S>(mut stream: S) -> io::Result<()>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  serve_with(&mut stream, execute).await
}

async fn serve_with<S, F, R>(mut stream: S, operation: F) -> io::Result<()>
where
  S: AsyncRead + AsyncWrite + Unpin,
  F: FnOnce(Request) -> R,
  R: Future<Output = io::Result<Response>>,
{
  let request = tokio::time::timeout(files::STARTUP_TIMEOUT, files::accept(&mut stream))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "File service setup timed out"))??;
  let Ok(_permit) = SERVICE_REQUESTS.try_acquire() else {
    return respond(&mut stream, &error_response(&busy())).await;
  };
  let (mut reader, mut writer) = tokio::io::split(stream);
  let result = tokio::select! {
    biased;
    closed = reader.read_u8() => {
      return match closed {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "File streams accept exactly one request")),
      };
    }
    result = operation(request) => result,
  };
  let response = result.unwrap_or_else(|error| error_response(&error));
  // A peer which stops reading must not leave a completed service task alive.
  respond(&mut writer, &response).await
}

async fn respond(writer: &mut (impl AsyncWrite + Unpin), response: &Response) -> io::Result<()> {
  tokio::time::timeout(files::STARTUP_TIMEOUT, files::respond(writer, response))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "File response timed out"))?
}

fn error_response(error: &io::Error) -> Response {
  let code = match error.kind() {
    io::ErrorKind::NotFound => "not_found",
    io::ErrorKind::PermissionDenied => "permission_denied",
    io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => "invalid_request",
    io::ErrorKind::Unsupported => "unsupported",
    io::ErrorKind::TimedOut => "timeout",
    io::ErrorKind::WouldBlock => "busy",
    _ => "file_error",
  };
  Response::Error {
    code: code.into(),
    message: error.to_string(),
  }
}

fn busy() -> io::Error {
  io::Error::new(
    io::ErrorKind::WouldBlock,
    "The file service has eight active operations; retry after one completes",
  )
}

fn resolve(path: &str, cwd: Option<&str>) -> io::Result<PathBuf> {
  let expanded =
    if path == "~" || path.starts_with("~/") || cfg!(windows) && path.starts_with("~\\") {
      let home = dirs::home_dir().ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::NotFound,
          "Account home directory is unavailable",
        )
      })?;
      if path == "~" {
        home
      } else {
        home.join(&path[2..])
      }
    } else {
      PathBuf::from(path)
    };
  if expanded.is_absolute() {
    return Ok(expanded);
  }
  let cwd = cwd
    .map(Path::new)
    .filter(|value| value.is_absolute())
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "Relative file paths require the originating pane's absolute working directory",
      )
    })?;
  Ok(cwd.join(expanded))
}

fn path_text(path: &Path) -> io::Result<String> {
  path
    .to_str()
    .filter(|path| path.len() <= files::MAX_PATH_BYTES)
    .map(str::to_owned)
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::Unsupported,
        "File paths must be bounded valid UTF-8",
      )
    })
}

async fn canonical(path: &Path) -> io::Result<PathBuf> {
  let path = fs::canonicalize(path).await?;
  path_text(&path)?;
  Ok(path)
}

async fn entry(path: &Path) -> io::Result<FileEntry> {
  let link_metadata = fs::symlink_metadata(path).await?;
  let is_symlink = link_metadata.is_symlink();
  let metadata = if is_symlink {
    match fs::metadata(path).await {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => link_metadata,
      Err(error) => return Err(error),
    }
  } else {
    link_metadata
  };
  if metadata.len() > (1_u64 << 53) - 1 {
    return Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "File size exceeds the supported integer range",
    ));
  }
  let entry_type = if metadata.is_file() {
    EntryType::File
  } else if metadata.is_dir() {
    EntryType::Directory
  } else {
    EntryType::Other
  };
  Ok(FileEntry {
    name: path
      .file_name()
      .and_then(|name| name.to_str())
      .unwrap_or("")
      .into(),
    path: path_text(path)?,
    entry_type,
    size: metadata.len(),
    is_symlink,
  })
}

async fn metadata(path: &Path) -> io::Result<Response> {
  let entry = match entry(path).await {
    Ok(mut entry) => {
      // A dangling symlink remains visible as an Other entry, but cannot be read.
      let resolved = match canonical(path).await {
        Ok(resolved) => resolved,
        Err(error) if entry.is_symlink && error.kind() == io::ErrorKind::NotFound => {
          path.to_owned()
        }
        Err(error) => return Err(error),
      };
      entry.path = path_text(&resolved)?;
      Some(entry)
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
    Err(error) => return Err(error),
  };
  let path = entry
    .as_ref()
    .map_or_else(|| path_text(path), |entry| Ok(entry.path.clone()))?;
  Ok(Response::Metadata { path, entry })
}

async fn read_file(path: &Path, offset: u64, length: u32) -> io::Result<Response> {
  let path = canonical(path).await?;
  if !fs::metadata(&path).await?.is_file() {
    return Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "Only regular files can be previewed",
    ));
  }
  let mut options = fs::OpenOptions::new();
  options.read(true);
  // A file replaced by a FIFO between metadata and open must not occupy an
  // uncancellable blocking worker. Regular files ignore O_NONBLOCK.
  #[cfg(unix)]
  options
    .custom_flags(i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).map_err(io::Error::other)?);
  let mut file = options.open(&path).await?;
  let metadata = file.metadata().await?;
  if !metadata.is_file() || metadata.len() > (1_u64 << 53) - 1 {
    return Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "Only bounded regular files can be previewed",
    ));
  }
  file.seek(io::SeekFrom::Start(offset)).await?;
  let mut data = Vec::with_capacity(length as usize);
  file.take(u64::from(length)).read_to_end(&mut data).await?;
  let eof =
    offset.saturating_add(data.len() as u64) >= metadata.len() || data.len() < length as usize;
  Ok(Response::File {
    path: path_text(&path)?,
    data_base64: STANDARD.encode(data),
    offset,
    total_size: metadata.len(),
    eof,
  })
}

async fn list_directory(path: &Path, offset: u32, limit: u32) -> io::Result<Response> {
  let path = canonical(path).await?;
  let mut directory = fs::read_dir(&path).await?;
  for _ in 0..offset {
    if directory.next_entry().await?.is_none() {
      return Ok(Response::Directory {
        path: path_text(&path)?,
        entries: Vec::new(),
        next_offset: None,
      });
    }
  }
  let mut entries = Vec::with_capacity(limit as usize);
  while entries.len() < limit as usize {
    let Some(next) = directory.next_entry().await? else {
      break;
    };
    entries.push(entry(&next.path()).await?);
  }
  let next_offset = if entries.len() == limit as usize && directory.next_entry().await?.is_some() {
    let next = offset + limit;
    if next > files::MAX_DIRECTORY_OFFSET {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Directory exceeds the supported pagination limit",
      ));
    }
    Some(next)
  } else {
    None
  };
  Ok(Response::Directory {
    path: path_text(&path)?,
    entries,
    next_offset,
  })
}

#[cfg(test)]
mod tests;
