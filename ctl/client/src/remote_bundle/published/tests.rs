use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct Server {
  root: reqwest::Url,
  requests: Arc<Mutex<Vec<String>>>,
  task: tokio::task::JoinHandle<()>,
}

impl Server {
  async fn start(routes: impl FnOnce(&reqwest::Url) -> BTreeMap<String, Vec<u8>>) -> Self {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = reqwest::Url::parse(&format!(
      "http://{}/bundles/",
      listener.local_addr().unwrap()
    ))
    .unwrap();
    let routes = routes(&root);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let task = tokio::spawn(async move {
      while let Ok((mut stream, _)) = listener.accept().await {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
          let mut byte = [0];
          if stream.read(&mut byte).await.unwrap_or(0) == 0 {
            break;
          }
          request.push(byte[0]);
          assert!(request.len() < 16 * 1024);
        }
        let request = String::from_utf8(request).unwrap();
        let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
        recorded.lock().unwrap().push(path.clone());
        let (status, body) = routes
          .get(&path)
          .map_or(("404 Not Found", &b"missing"[..]), |body| {
            ("200 OK", body.as_slice())
          });
        let header = format!(
          "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
          body.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
      }
    });
    Self {
      root,
      requests,
      task,
    }
  }

  fn resolver(&self) -> Resolver {
    Resolver::new(self.root.clone(), false).unwrap()
  }
}
impl Drop for Server {
  fn drop(&mut self) {
    self.task.abort();
  }
}

fn fixture() -> Fixture {
  Fixture::new("0.1.0", &"a".repeat(40))
}

async fn server(pointer: impl FnOnce(&reqwest::Url) -> serde_json::Value, corrupt: bool) -> Server {
  let fixture = fixture();
  let archive = fixture.archive();
  let outer = fixture.bind_archive(&archive);
  Server::start(move |root| {
    let name = fixture.outer["targets"]["aarch64-apple-darwin"]["archive"]
      .as_str()
      .unwrap();
    BTreeMap::from([
      (
        "/bundles/channels/dev.json".to_owned(),
        serde_json::to_vec(&pointer(root)).unwrap(),
      ),
      ("/bundles/sets/test/bundle-set.json".to_owned(), outer),
      (
        format!("/bundles/sets/test/{name}"),
        if corrupt {
          b"corrupt".to_vec()
        } else {
          archive
        },
      ),
    ])
  })
  .await
}

fn pointer(root: &reqwest::Url) -> serde_json::Value {
  serde_json::json!({"schema_version":1,"channel":"dev","bundle_set_url":root.join("sets/test/bundle-set.json").unwrap().as_str()})
}

#[tokio::test]
async fn check_reads_only_channel_and_metadata_while_download_verifies_archive() {
  let server = server(pointer, false).await;
  let resolver = server.resolver();
  let catalog = resolver.check(BundleChannel::Dev).await.unwrap();
  assert_eq!(catalog.git_revision, "a".repeat(40));
  assert_eq!(catalog.targets.len(), 4);
  assert!(catalog.targets.iter().all(|target| target.compatible));
  assert_eq!(
    server.requests.lock().unwrap().as_slice(),
    [
      "/bundles/channels/dev.json",
      "/bundles/sets/test/bundle-set.json"
    ]
  );
  let bundle = resolver
    .download(BundleChannel::Dev, "aarch64-apple-darwin")
    .await
    .unwrap();
  assert_eq!(bundle.git_revision, catalog.git_revision);
  assert!(
    server
      .requests
      .lock()
      .unwrap()
      .last()
      .unwrap()
      .ends_with(".tar.gz")
  );
}

#[tokio::test]
async fn channel_identity_urls_and_archive_checksums_fail_closed() {
  for change in ["channel", "origin", "path", "checksum"] {
    let server = server(
      |root| {
        let mut value = pointer(root);
        match change {
          "channel" => value["channel"] = "stable".into(),
          "origin" => value["bundle_set_url"] = "https://example.com/bundle-set.json".into(),
          "path" => {
            value["bundle_set_url"] = root
              .join("../other/bundle-set.json")
              .unwrap()
              .as_str()
              .into();
          }
          _ => {}
        }
        value
      },
      change == "checksum",
    )
    .await;
    assert!(
      server
        .resolver()
        .download(BundleChannel::Dev, "aarch64-apple-darwin")
        .await
        .is_err(),
      "{change}"
    );
  }
}

#[test]
fn production_urls_are_restricted_to_the_fixed_https_publication_tree() {
  let root = reqwest::Url::parse(ROOT).unwrap();
  for url in [
    "https://tokn-ai.github.io/ctl/bundles/sets/id/bundle-set.json",
    "http://tokn-ai.dev/ctl/bundles/sets/id/bundle-set.json",
    "https://tokn-ai.dev/other/bundle-set.json",
    "https://tokn-ai.dev/ctl/bundles/sets/a%2fb/bundle-set.json",
    "https://tokn-ai.dev/ctl/bundles/sets/id/bundle-set.json?redirect=other",
    "https://user@tokn-ai.dev/ctl/bundles/sets/id/bundle-set.json",
  ] {
    assert!(!trusted(&reqwest::Url::parse(url).unwrap(), &root), "{url}");
  }
}

#[tokio::test]
async fn exact_publication_cache_avoids_archive_redownload_but_not_fresh_discovery() {
  use sha2::{Digest as _, Sha256};
  let root = std::env::temp_dir().join(format!("ctl-published-{}", uuid::Uuid::new_v4()));
  let builder = std::fs::DirBuilder::new();
  #[cfg(unix)]
  let mut builder = builder;
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  builder.create(&root).unwrap();
  let server = server(pointer, false).await;
  let resolver = server.resolver();
  let target = "aarch64-apple-darwin";
  let first = resolver
    .download_cached(BundleChannel::Dev, target, &root)
    .await
    .unwrap();
  let second = resolver
    .download_cached(BundleChannel::Dev, target, &root)
    .await
    .unwrap();
  assert_eq!(first.archive, second.archive);
  {
    let requests = server.requests.lock().unwrap();
    assert_eq!(
      requests
        .iter()
        .filter(|path| path.ends_with(".tar.gz"))
        .count(),
      1
    );
    assert_eq!(
      requests
        .iter()
        .filter(|path| path.ends_with("dev.json"))
        .count(),
      2
    );
  }
  let cached = root
    .join(format!("{:x}", Sha256::digest(&first.manifest)))
    .join(target)
    .join(&first.file_name);
  std::fs::write(cached, b"corrupt cached archive").unwrap();
  assert_eq!(
    resolver
      .download_cached(BundleChannel::Dev, target, &root)
      .await
      .unwrap()
      .archive,
    first.archive
  );
  assert_eq!(
    server
      .requests
      .lock()
      .unwrap()
      .iter()
      .filter(|path| path.ends_with(".tar.gz"))
      .count(),
    2
  );
  drop(server);
  tokio::task::yield_now().await;
  assert!(
    resolver
      .download_cached(BundleChannel::Dev, target, &root)
      .await
      .is_err(),
    "a cached archive must not replace failed channel discovery"
  );
  std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn channel_metadata_is_bounded_before_parsing() {
  let server = Server::start(|_| {
    BTreeMap::from([(
      "/bundles/channels/dev.json".to_owned(),
      vec![b'x'; MAX_CHANNEL_BYTES + 1],
    )])
  })
  .await;
  assert!(server.resolver().check(BundleChannel::Dev).await.is_err());
  assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stable_channel_rejects_prerelease_product_versions() {
  let fixture = Fixture::new("0.1.0-beta.1", &"a".repeat(40));
  let server = Server::start(|root| BTreeMap::from([
    ("/bundles/channels/stable.json".into(), serde_json::to_vec(&serde_json::json!({
      "schema_version":1, "channel":"stable", "bundle_set_url":root.join("sets/test/bundle-set.json").unwrap().as_str(),
    })).unwrap()),
    ("/bundles/sets/test/bundle-set.json".into(), serde_json::to_vec(&fixture.outer).unwrap()),
  ])).await;
  assert!(
    matches!(server.resolver().check(BundleChannel::Stable).await, Err(Error::Invalid(message)) if message.contains("prerelease"))
  );
}
