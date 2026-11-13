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
    Self::start_with(|root| {
      let routes = routes(root);
      move |path: &str| routes.get(path).cloned()
    })
    .await
  }

  async fn start_with<F>(respond: impl FnOnce(&reqwest::Url) -> F) -> Self
  where
    F: FnMut(&str) -> Option<Vec<u8>> + Send + 'static,
  {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = reqwest::Url::parse(&format!(
      "http://{}/bundles/",
      listener.local_addr().unwrap()
    ))
    .unwrap();
    let mut respond = respond(&root);
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
        let body = respond(&path);
        let (status, body) = body
          .as_ref()
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
        "/bundles/channels/nightly.json".to_owned(),
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
  serde_json::json!({"schema_version":1,"channel":"nightly","bundle_set_url":root.join("sets/test/bundle-set.json").unwrap().as_str()})
}

fn retained_release(channel: &str, version: &str, run_number: u64) -> serde_json::Value {
  let revision = "a".repeat(40);
  let bundle_id = match channel {
    "stable" => version.to_owned(),
    "dev" => format!("{version}-dev.{}", &revision[..12]),
    _ => format!("{version}-nightly.20261110+g{}", &revision[..12]),
  };
  serde_json::json!({
    "channel": channel,
    "publication_id": format!("{bundle_id}-{run_number}-1"),
    "bundle_id": bundle_id,
    "app_version": version,
    "git_revision": revision,
    "run_id": run_number.to_string(),
    "run_number": run_number,
    "run_attempt": 1,
  })
}

#[tokio::test]
async fn release_history_lists_retained_channels_and_accepts_a_legacy_nightly_head() {
  let first = retained_release("dev", "0.1.0", 1);
  let latest = retained_release("dev", "0.1.0", 2);
  let stable = retained_release("stable", "0.1.0", 3);
  let prerelease = retained_release("stable", "0.1.1-beta.1", 4);
  let catalog = serde_json::json!({
    "schema_version": 1,
    "site": "ctl-bundle-pages",
    "generation": 4,
    "channels": {"dev": latest["publication_id"], "stable": stable["publication_id"]},
    "publications": [first, stable, prerelease, latest],
  });
  let server = Server::start(|_| {
    BTreeMap::from([(
      "/bundles/catalog.json".into(),
      serde_json::to_vec(&catalog).unwrap(),
    )])
  })
  .await;
  let resolver = server.resolver();

  let nightly = resolver
    .list_releases(BundleChannel::Nightly)
    .await
    .unwrap();
  assert_eq!(nightly.len(), 2);
  assert_eq!(
    nightly[0].publication_id,
    latest["publication_id"].as_str().unwrap()
  );
  assert_eq!(nightly[0].channel, BundleChannel::Nightly);
  assert!(nightly[0].current);
  assert!(!nightly[1].current);

  let stable_releases = resolver.list_releases(BundleChannel::Stable).await.unwrap();
  assert_eq!(stable_releases.len(), 1);
  assert_eq!(stable_releases[0].app_version, "0.1.0");
  assert!(stable_releases[0].current);
  assert_eq!(
    server.requests.lock().unwrap().as_slice(),
    ["/bundles/catalog.json", "/bundles/catalog.json"]
  );
}

#[tokio::test]
async fn release_view_fetches_only_the_catalog_and_exact_manifest_and_checks_identity() {
  let nightly = retained_release("nightly", "0.1.0", 1);
  let id = nightly["publication_id"].as_str().unwrap();
  let bundle_id = nightly["bundle_id"].as_str().unwrap();
  let catalog = serde_json::json!({
    "schema_version": 1,
    "site": "ctl-bundle-pages",
    "generation": 1,
    "channels": {"nightly": id},
    "publications": [nightly],
  });
  let manifest_path = format!("/bundles/sets/{id}/bundle-set.json");

  for (revision, succeeds) in [("a".repeat(40), true), ("b".repeat(40), false)] {
    let mut fixture = Fixture::new("0.1.0", &revision);
    let revision_id = format!("0.1.0-nightly.20261110+g{}", &revision[..12]);
    fixture.outer["bundle_id"] = serde_json::json!(revision_id);
    for (target, value) in fixture.outer["targets"].as_object_mut().unwrap() {
      value["archive"] =
        serde_json::json!(format!("ctl-agent-bundle-{revision_id}-{target}.tar.gz"));
    }
    let server = Server::start(|_| {
      BTreeMap::from([
        (
          "/bundles/catalog.json".into(),
          serde_json::to_vec(&catalog).unwrap(),
        ),
        (
          manifest_path.clone(),
          fixture.bind_archive(&fixture.archive()),
        ),
      ])
    })
    .await;
    let resolver = server.resolver();
    let detail = resolver
      .view_release(BundleChannel::Nightly, succeeds.then_some(id))
      .await;
    if succeeds {
      let detail = detail.unwrap();
      assert_eq!(detail.release.publication_id, id);
      assert_eq!(detail.release.bundle_id, bundle_id);
      assert_eq!(detail.targets.len(), 4);
      assert!(detail.targets.iter().all(|target| target.compatible));
    } else {
      assert!(
        matches!(detail, Err(Error::Invalid(message)) if message.contains("identities differ"))
      );
    }
    assert!(matches!(
      resolver
        .view_release(BundleChannel::Nightly, Some("../../other"))
        .await,
      Err(Error::NotAvailable(_))
    ));
    let mut expected = vec!["/bundles/catalog.json".into(), manifest_path.clone()];
    if succeeds {
      expected.push(format!("/bundles/sets/{id}/{WINDOWS_BUNDLE_SET_FILE}"));
    }
    expected.push("/bundles/catalog.json".into());
    assert_eq!(*server.requests.lock().unwrap(), expected);
  }
}

#[tokio::test]
async fn check_names_the_channel_when_its_pointer_is_missing() {
  let server = Server::start(|_| BTreeMap::new()).await;
  let resolver = server.resolver();
  for channel in [BundleChannel::Stable, BundleChannel::Nightly] {
    assert!(matches!(
      resolver.check(channel).await,
      Err(Error::NotAvailable(message)) if message == format!("the {} channel is not published", channel.name())
    ));
  }
  assert_eq!(
    server.requests.lock().unwrap().as_slice(),
    [
      "/bundles/channels/stable.json",
      "/bundles/channels/nightly.json"
    ]
  );
}

#[tokio::test]
async fn check_reads_only_channel_and_metadata_while_download_verifies_archive() {
  let server = server(pointer, false).await;
  let resolver = server.resolver();
  let catalog = resolver.check(BundleChannel::Nightly).await.unwrap();
  assert_eq!(catalog.git_revision, "a".repeat(40));
  assert_eq!(catalog.targets.len(), 4);
  assert!(catalog.targets.iter().all(|target| target.compatible));
  assert_eq!(
    server.requests.lock().unwrap().as_slice(),
    [
      "/bundles/channels/nightly.json",
      "/bundles/sets/test/bundle-set.json",
      "/bundles/sets/test/bundle-set-windows.json"
    ]
  );
  let bundle = resolver
    .download(BundleChannel::Nightly, "aarch64-apple-darwin")
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
async fn nightly_discovery_ignores_the_legacy_dev_pointer() {
  let server = Server::start(move |root| {
    let mut dev = pointer(root);
    dev["channel"] = "dev".into();
    BTreeMap::from([(
      "/bundles/channels/dev.json".into(),
      serde_json::to_vec(&dev).unwrap(),
    )])
  })
  .await;
  assert!(matches!(
    server.resolver().check(BundleChannel::Nightly).await,
    Err(Error::NotAvailable(message)) if message == "the nightly channel is not published"
  ));
  assert_eq!(
    server.requests.lock().unwrap().as_slice(),
    ["/bundles/channels/nightly.json"]
  );
}

#[tokio::test]
async fn channel_identity_urls_and_archive_checksums_fail_closed() {
  for change in ["channel", "origin", "path", "checksum"] {
    let server = server(
      |root| {
        let mut value = pointer(root);
        match change {
          "channel" => value["channel"] = "dev".into(),
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
        .download(BundleChannel::Nightly, "aarch64-apple-darwin")
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
    .download_cached(BundleChannel::Nightly, target, &root)
    .await
    .unwrap();
  let second = resolver
    .download_cached(BundleChannel::Nightly, target, &root)
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
        .filter(|path| path.ends_with("nightly.json"))
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
      .download_cached(BundleChannel::Nightly, target, &root)
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
      .download_cached(BundleChannel::Nightly, target, &root)
      .await
      .is_err(),
    "a cached archive must not replace failed channel discovery"
  );
  std::fs::remove_dir_all(root).unwrap();
}

fn installation_routes(
  root: &reqwest::Url,
  target: &str,
  id: &str,
  version: &str,
  revision: &str,
) -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
  use sha2::{Digest as _, Sha256};
  let fixture = Fixture::new(version, revision).with_clients();
  let manifest = fixture.bind_archive(&fixture.archive());
  let fixture = if target == WINDOWS_TARGET {
    fixture.for_windows()
  } else {
    fixture
  };
  let archive = fixture.archive();
  let archive_name = fixture.outer["targets"][target]["archive"]
    .as_str()
    .unwrap();
  let metadata_url = root.join(&format!("sets/{id}/bundle-set.json")).unwrap();
  let pointer = serde_json::to_vec(&serde_json::json!({
    "schema_version": 1, "channel":"nightly", "bundle_set_url": metadata_url.as_str(),
  }))
  .unwrap();
  let updater = format!("fixture updater for {id}").into_bytes();
  let binary =
    ctl_core::bundle_layout::executable_name(target, &format!("ctlup-{version}-{target}"))
      .into_owned();
  let sidecar = UpdaterInfo {
    schema_version: 1,
    app_version: version.into(),
    bundle_id: version.into(),
    git_revision: revision.into(),
    target_triple: target.into(),
    binary: binary.clone(),
    sha256: format!("{:x}", Sha256::digest(&updater)),
    size: updater.len() as u64,
  };
  let windows_manifest = (target == WINDOWS_TARGET).then(|| fixture.bind_archive(&archive));
  let mut routes = BTreeMap::from([
    (metadata_url.path().to_owned(), manifest),
    (format!("/bundles/sets/{id}/{archive_name}"), archive),
    (
      format!("/bundles/sets/{id}/ctlup-{target}.json"),
      serde_json::to_vec(&sidecar).unwrap(),
    ),
    (format!("/bundles/sets/{id}/{binary}"), updater),
  ]);
  if let Some(windows_manifest) = windows_manifest {
    let metadata_url = metadata_url.join(WINDOWS_BUNDLE_SET_FILE).unwrap();
    routes.insert(metadata_url.path().to_owned(), windows_manifest);
  }
  (pointer, routes)
}

#[tokio::test]
async fn installation_download_pins_both_assets_when_the_channel_advances() {
  let home = std::env::temp_dir().join(format!(
    "ctl-installation-publication-{}",
    uuid::Uuid::new_v4()
  ));
  ctl_core::private_fs::create_directory(&home).unwrap();
  let target = "aarch64-apple-darwin";
  let channel_path = "/bundles/channels/nightly.json";
  let first_revision = "a".repeat(40);
  let second_revision = "b".repeat(40);
  let server = Server::start_with(|root| {
    let (initial, mut routes) =
      installation_routes(root, target, "first", "0.1.0", &first_revision);
    let (next, next_routes) =
      installation_routes(root, target, "second", "0.1.1", &second_revision);
    routes.extend(next_routes);
    let mut initial = Some(initial);
    move |path: &str| {
      if path == channel_path {
        Some(initial.take().unwrap_or_else(|| next.clone()))
      } else {
        routes.get(path).cloned()
      }
    }
  })
  .await;
  let cache = home.join(".tokn/ctl/cache");
  let progress = Mutex::new(Vec::new());
  let (bundle, updater) = server
    .resolver()
    .download_installation_cached(
      BundleChannel::Nightly,
      target,
      &cache.join("downloads"),
      &cache.join("updaters"),
      &|event| progress.lock().unwrap().push(event),
    )
    .await
    .unwrap();
  assert_eq!(bundle.git_revision, first_revision);
  assert_eq!(updater.info.git_revision, bundle.git_revision);
  assert_eq!(updater.bytes, b"fixture updater for first");
  for (artifact, size) in [
    (DownloadArtifact::Updater, updater.info.size),
    (DownloadArtifact::Runtime, bundle.archive.len() as u64),
  ] {
    let progress = progress.lock().unwrap();
    let complete = progress
      .iter()
      .rfind(|event| event.artifact == artifact)
      .unwrap();
    assert_eq!(
      (complete.received_bytes, complete.total_bytes),
      (size, size)
    );
  }
  {
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[0], channel_path);
    assert_eq!(requests.len(), 5);
    assert!(
      requests[1..]
        .iter()
        .all(|path| path.starts_with("/bundles/sets/first/"))
    );
  }
  let current = server
    .resolver()
    .check(BundleChannel::Nightly)
    .await
    .unwrap();
  assert_eq!(current.git_revision, second_revision);
  std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn windows_installation_resolves_both_assets_from_its_companion_set() {
  let revision = "a".repeat(40);
  let server = Server::start(|root| {
    let (pointer, mut routes) =
      installation_routes(root, WINDOWS_TARGET, "first", "0.1.0", &revision);
    routes.insert("/bundles/channels/nightly.json".into(), pointer);
    routes
  })
  .await;
  let home = crate::installation::TemporaryDirectory::new("windows-publication-test").unwrap();
  let cache = home.0.join(".tokn/ctl/cache");
  let resolver = server.resolver();
  let (bundle, updater) = resolver
    .download_installation_cached(
      BundleChannel::Nightly,
      WINDOWS_TARGET,
      &cache.join("downloads"),
      &cache.join("updaters"),
      &|_| {},
    )
    .await
    .unwrap();
  assert_eq!(
    updater.info.binary,
    format!("ctlup-0.1.0-{WINDOWS_TARGET}.exe")
  );
  assert_eq!(bundle.git_revision, updater.info.git_revision);
  let catalog = resolver.check(BundleChannel::Nightly).await.unwrap();
  assert!(
    catalog
      .targets
      .iter()
      .any(|target| target.target_triple == WINDOWS_TARGET && target.compatible)
  );
}

#[cfg(unix)]
#[tokio::test]
async fn missing_installation_updater_is_reported_before_downloading_runtime() {
  let server = server(pointer, false).await;
  let unused = Path::new("");
  let result = server
    .resolver()
    .download_installation_cached(
      BundleChannel::Nightly,
      "aarch64-apple-darwin",
      unused,
      unused,
      &|_| {},
    )
    .await;
  assert!(
    matches!(result, Err(Error::NotAvailable(message)) if message.contains("0.1.0 has no ctlup metadata for aarch64-apple-darwin"))
  );
  assert_eq!(
    server.requests.lock().unwrap().as_slice(),
    [
      "/bundles/channels/nightly.json",
      "/bundles/sets/test/bundle-set.json",
      "/bundles/sets/test/ctlup-aarch64-apple-darwin.json",
    ]
  );
}

#[tokio::test]
async fn channel_metadata_is_bounded_before_parsing() {
  let server = Server::start(|_| {
    BTreeMap::from([(
      "/bundles/channels/nightly.json".to_owned(),
      vec![b'x'; MAX_CHANNEL_BYTES + 1],
    )])
  })
  .await;
  assert!(
    server
      .resolver()
      .check(BundleChannel::Nightly)
      .await
      .is_err()
  );
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
