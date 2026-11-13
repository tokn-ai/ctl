use super::*;
use std::sync::{
  Arc,
  atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
  async fn new() -> Self {
    let directory = std::env::temp_dir().join(format!(
      "ctl-files-fixture-{}-{}",
      std::process::id(),
      FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).await.unwrap();
    Self(directory)
  }
  fn path(&self, name: &str) -> String {
    self.0.join(name).to_str().unwrap().into()
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
async fn ranged_reads_are_bounded_and_relative_paths_keep_their_cwd() {
  let fixture = Fixture::new().await;
  fs::write(fixture.0.join("file.txt"), b"0123456789")
    .await
    .unwrap();
  let result = execute(Request::ReadFile {
    path: "file.txt".into(),
    cwd: Some(fixture.0.to_str().unwrap().into()),
    offset: 3,
    length: 4,
  })
  .await
  .unwrap();
  let Response::File {
    data_base64,
    offset,
    total_size,
    eof,
    ..
  } = result
  else {
    panic!("expected file");
  };
  assert_eq!(STANDARD.decode(data_base64).unwrap(), b"3456");
  assert_eq!((offset, total_size, eof), (3, 10, false));
  assert!(
    execute(Request::Metadata {
      path: "file.txt".into(),
      cwd: None
    })
    .await
    .is_err()
  );
  let result = execute(Request::Metadata {
    path: fixture.path("missing"),
    cwd: None,
  })
  .await
  .unwrap();
  assert!(matches!(result, Response::Metadata { entry: None, .. }));
}

#[tokio::test]
async fn paged_directories_do_not_drop_entries() {
  let fixture = Fixture::new().await;
  for name in ["one", "two", "three"] {
    fs::write(fixture.0.join(name), b"content").await.unwrap();
  }
  let path = fixture.0.to_str().unwrap().to_owned();
  let Response::Directory {
    entries: first,
    next_offset,
    ..
  } = execute(Request::ListDirectory {
    path: path.clone(),
    cwd: None,
    offset: 0,
    limit: 2,
  })
  .await
  .unwrap()
  else {
    panic!("expected directory");
  };
  let Response::Directory {
    entries: second,
    next_offset: finished,
    ..
  } = execute(Request::ListDirectory {
    path,
    cwd: None,
    offset: next_offset.unwrap(),
    limit: 2,
  })
  .await
  .unwrap()
  else {
    panic!("expected directory");
  };
  assert_eq!(first.len(), 2);
  assert_eq!(second.len(), 1);
  assert_eq!(finished, None);
  let mut names = first
    .into_iter()
    .chain(second)
    .map(|entry| entry.name)
    .collect::<Vec<_>>();
  names.sort();
  assert_eq!(names, ["one", "three", "two"]);
}

struct DropMarker(Arc<AtomicBool>);
impl Drop for DropMarker {
  fn drop(&mut self) {
    self.0.store(true, Ordering::Release);
  }
}

#[tokio::test]
async fn peer_closure_cancels_its_operation_while_a_sibling_completes() {
  let fixture = Fixture::new().await;
  let (cancel_client, cancel_server) = tokio::io::duplex(8192);
  let (sibling_client, sibling_server) = tokio::io::duplex(8192);
  let dropped = Arc::new(AtomicBool::new(false));
  let observed_drop = Arc::clone(&dropped);
  let started = Arc::new(tokio::sync::Notify::new());
  let operation_started = Arc::clone(&started);
  let server = tokio::spawn(serve_with(cancel_server, move |_| async move {
    let _drop = DropMarker(observed_drop);
    operation_started.notify_one();
    std::future::pending::<io::Result<Response>>().await
  }));
  let sibling = tokio::spawn(serve(sibling_server));
  let request = Request::Metadata {
    path: fixture.path("missing"),
    cwd: None,
  };
  let cancel_request = request.clone();
  let client = tokio::spawn(async move { files::request(cancel_client, &cancel_request).await });
  tokio::time::timeout(Duration::from_secs(2), started.notified())
    .await
    .unwrap();
  client.abort();
  let _ = client.await;
  tokio::time::timeout(Duration::from_secs(2), server)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  assert!(dropped.load(Ordering::Acquire));
  assert!(matches!(
    files::request(sibling_client, &request).await.unwrap(),
    Response::Metadata { entry: None, .. }
  ));
  sibling.await.unwrap().unwrap();
}

#[tokio::test]
async fn git_status_reports_untracked_paths_without_shell_interpolation() {
  let fixture = Fixture::new().await;
  let status = tokio::process::Command::new("git")
    .args(["init", "--quiet"])
    .arg(&fixture.0)
    .status()
    .await
    .unwrap();
  assert!(status.success());
  let name = if cfg!(windows) {
    "literal ; file name"
  } else {
    "literal ; file\nname"
  };
  fs::write(fixture.0.join(name), b"content").await.unwrap();
  let Response::GitStatus { entries, .. } = execute(Request::GitStatus {
    path: fixture.0.to_str().unwrap().into(),
    cwd: None,
  })
  .await
  .unwrap() else {
    panic!("expected git status");
  };
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0].path, name);
  assert_eq!(entries[0].index_status, "?");
  assert_eq!(entries[0].worktree_status, "?");
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_preserve_visibility_and_preview_the_resolved_regular_file() {
  use std::os::unix::fs::symlink;
  let fixture = Fixture::new().await;
  fs::write(fixture.0.join("target"), b"contents")
    .await
    .unwrap();
  symlink("target", fixture.0.join("alias")).unwrap();
  symlink("missing", fixture.0.join("dangling")).unwrap();
  let Response::Metadata {
    entry: Some(entry), ..
  } = execute(Request::Metadata {
    path: fixture.path("alias"),
    cwd: None,
  })
  .await
  .unwrap()
  else {
    panic!("expected metadata");
  };
  assert!(entry.is_symlink);
  assert_eq!(entry.entry_type, EntryType::File);
  let result = execute(Request::ReadFile {
    path: fixture.path("alias"),
    cwd: None,
    offset: 0,
    length: 1024,
  })
  .await
  .unwrap();
  assert!(matches!(result, Response::File { eof: true, .. }));
  let Response::Metadata {
    entry: Some(entry), ..
  } = execute(Request::Metadata {
    path: fixture.path("dangling"),
    cwd: None,
  })
  .await
  .unwrap()
  else {
    panic!("expected metadata");
  };
  assert_eq!(entry.entry_type, EntryType::Other);
}
