use super::*;

#[tokio::test]
async fn sender_reports_partial_uploads_and_defers_to_newer_receiver_stages() {
  use PublishedInstallStage as Stage;

  let bytes = vec![0x42; 3 * 64 * 1024];
  let total = bytes.len() as u64;
  for advance_stage in [false, true] {
    let (mut sender, mut receiver) = tokio::io::duplex(4096);
    let (activity, _observer) = tokio::sync::watch::channel(Stage::UploadingRuntime);
    let mut progress = Vec::new();
    let write = write_asset(
      &mut sender,
      &bytes,
      &activity,
      Stage::UploadingRuntime,
      |sent, total| {
        progress.push((sent, total));
      },
    );
    let read = async {
      let mut payload = vec![0; bytes.len()];
      receiver.read_exact(&mut payload[..1]).await.unwrap();
      // Backpressure keeps the first write pending beyond the redraw window.
      if advance_stage {
        activity.send_replace(Stage::ReusedRuntime);
      } else {
        tokio::time::sleep(Duration::from_millis(275)).await;
      }
      receiver.read_exact(&mut payload[1..]).await.unwrap();
      assert_eq!(payload, bytes);
    };
    let (result, ()) = tokio::join!(write, read);
    result.unwrap();
    if advance_stage {
      assert_eq!(progress, [(0, total)]);
    } else {
      assert_eq!(progress.first(), Some(&(0, total)));
      assert!(progress.iter().any(|&(sent, size)| sent > 0 && sent < size));
      assert_eq!(progress.last(), Some(&(total, total)));
    }
  }
}

#[test]
fn windows_bootstrap_keeps_pinned_metadata_off_the_command_line_and_fits_cmd() {
  let args = [
    "134217728",
    &"a".repeat(64),
    "https://assets.test/immutable/path/ctlup.exe",
    &"bf".repeat(540),
    "https://assets.test/immutable/path/runtime.tar.gz",
    "x86_64-pc-windows-msvc",
    "65536",
    "1",
    "0",
  ];
  let script = script(
    RemotePlatform::Windows,
    Some("aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"),
    &args,
  )
  .unwrap();
  let command = windows::powershell_command(&script).unwrap();
  assert!(command.len() < 8000);
  assert!(!script.contains("__"));
  assert!(script.contains("SetAccessRuleProtection($true, $false)"));
  assert!(script.contains("Copy-UpdaterOutput $manager ('export-manager --sha256 ' + $sha)"));
  assert!(script.contains("Assert-PlainUpdaterPath $path"));
  assert!(script.contains("Get-FileHash -LiteralPath $file"));
  assert!(script.contains("--manifest-bytes"));
}

#[cfg(unix)]
mod unix {
  use super::*;
  use crate::remote_bundle::compatibility::tests::Fixture;
  use sha2::{Digest, Sha256};
  use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Mutex};

  const TARGET: &str = "aarch64-unknown-linux-musl";
  const URL: &str = "https://assets.test/pinned/ctl'up";
  const ARCHIVE: &str = "https://assets.test/pinned/bundle?a=1&b=2";
  const MANIFEST: &[u8] = br#"{"original": true}"#;
  const HELPER: &str = r#"#!/bin/sh
set -eu
if [ "$1" = installation-layout-version ]; then printf '__LAYOUT_VERSION__\n'; exit 0; fi
if [ "$2" = --help ]; then
  if [ -f "$HOME/old" ]; then echo "error: unrecognized subcommand 'install-publication'" >&2; exit 2; fi
  if [ -f "$HOME/broken" ]; then echo 'failed to inspect executable' >&2; exit 2; fi
  exit 0
fi
printf '%s\n' "$@" > "$HOME/arguments"
cat > "$HOME/received"
if [ -f "$HOME/missing-runtime" ]; then printf 'ctl-published-unavailable-v1\n'; exit 12; fi
if [ -f "$HOME/forged" ]; then exit 12; fi
if [ -f "$HOME/reused" ]; then printf 'ctl-published-reused-v1\n'; fi
printf 'ctl-published-install-v1\n'
cat "$HOME/managed-manifest"
"#;

  fn helper() -> String {
    HELPER.replace(
      "__LAYOUT_VERSION__",
      &ctl_core::installations::DEVELOPMENT_LAYOUT_VERSION.to_string(),
    )
  }

  struct Host(PathBuf);

  impl Host {
    fn new() -> Self {
      let home = std::env::temp_dir().join(format!("ctl-published-{}", uuid::Uuid::new_v4()));
      fs::create_dir(&home).unwrap();
      fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
      fs::write(home.join("manager"), helper()).unwrap();
      let fixture = Fixture::new("0.1.0", &"b".repeat(40)).with_clients();
      let components = serde_json::from_value(fixture.inner["components"].clone()).unwrap();
      let manifest = Manifest::new(
        TARGET,
        ctl_core::bundles::Source::Ci,
        components,
        &fixture.files,
      )
      .unwrap();
      fs::write(
        home.join("managed-manifest"),
        serde_json::to_string(&manifest).unwrap() + "\n",
      )
      .unwrap();
      let host = Self(home);
      for name in ["curl", "wget"] {
        host.downloader(name, 0);
      }
      host
    }

    fn downloader(&self, name: &str, status: u8) {
      let path = self.0.join(name);
      let text = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$HOME/{name}-args\"\nif [ {status} != 0 ]; then exit {status}; fi\nwhile [ \"$1\" != --output ] && [ \"$1\" != -O ]; do shift; done\ncp \"$HOME/manager\" \"$2\"\n"
      );
      fs::write(&path, text).unwrap();
      fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    async fn install(
      &self,
      updater: Option<&[u8]>,
      runtime: Option<&[u8]>,
    ) -> (
      Result<Option<Manifest>, CoreError>,
      Vec<PublishedInstallStage>,
    ) {
      let helper = helper();
      let args: [&str; 9] = [
        &helper.len().to_string(),
        &hex::encode(Sha256::digest(&helper)),
        URL,
        "abcd",
        ARCHIVE,
        TARGET,
        &MANIFEST.len().to_string(),
        if updater.is_some() { "1" } else { "0" },
        if runtime.is_some() { "1" } else { "0" },
      ];
      let script = script(RemotePlatform::Unix, None, &args).unwrap();
      let mut command = tokio::process::Command::new("sh");
      command
        .arg("-c")
        .arg(script)
        .env("HOME", &self.0)
        .env("PATH", format!("{}:/usr/bin:/bin", self.0.display()));
      let events = Mutex::new(Vec::new());
      let result = tokio::time::timeout(
        Duration::from_secs(10),
        run(command, MANIFEST, updater, runtime, |stage| {
          events.lock().unwrap().push(stage);
        }),
      )
      .await
      .unwrap();
      let staging = self.0.join(".tokn/ctl/ctlup/staging");
      if staging.is_dir() {
        assert!(!fs::read_dir(staging).unwrap().any(|entry| {
          entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ctlup-fetch.")
        }));
      }
      (result, events.into_inner().unwrap())
    }
  }

  impl Drop for Host {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[tokio::test]
  async fn remote_fetch_uses_curl_then_wget_preserving_exact_input_and_arguments() {
    for use_wget in [true, false] {
      let host = Host::new();
      if use_wget {
        host.downloader("curl", 7);
      }
      let (result, phases) = host.install(None, None).await;
      result.unwrap().unwrap().validate().unwrap();
      assert_eq!(
        phases,
        [
          PublishedInstallStage::CheckingUpdater,
          PublishedInstallStage::DownloadingUpdater,
          PublishedInstallStage::InstallingRuntime
        ]
      );
      assert_eq!(fs::read(host.0.join("received")).unwrap(), MANIFEST);
      let args = fs::read_to_string(host.0.join("arguments")).unwrap();
      assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
          "install-publication",
          "--origin-hex",
          "abcd",
          "--archive-url",
          ARCHIVE,
          "--target",
          TARGET,
          "--manifest-bytes",
          &MANIFEST.len().to_string()
        ]
      );
      let curl = fs::read_to_string(host.0.join("curl-args")).unwrap();
      assert_eq!(curl.lines().last(), Some(URL));
      assert_eq!(host.0.join("wget-args").exists(), use_wget);
    }
  }

  #[tokio::test]
  async fn only_pre_activation_availability_or_old_capability_permit_fallback() {
    let host = Host::new();
    host.downloader("curl", 60);
    assert!(host.install(None, None).await.0.is_err());
    assert!(!host.0.join("wget-args").exists());
    host.downloader("curl", 2);
    assert!(host.install(None, None).await.0.unwrap().is_some());
    host.downloader("curl", 7);
    host.downloader("wget", 4);
    assert!(host.install(None, None).await.0.unwrap().is_none());
    host.downloader("curl", 0);
    fs::write(host.0.join("old"), b"").unwrap();
    assert!(host.install(None, None).await.0.unwrap().is_none());
    fs::remove_file(host.0.join("old")).unwrap();
    fs::write(host.0.join("broken"), b"").unwrap();
    assert!(host.install(None, None).await.0.is_err());
    fs::remove_file(host.0.join("broken")).unwrap();
    fs::write(host.0.join("forged"), b"").unwrap();
    assert!(host.install(None, None).await.0.is_err());
    fs::remove_file(host.0.join("forged")).unwrap();
    fs::write(host.0.join("missing-runtime"), b"").unwrap();
    assert!(host.install(None, None).await.0.unwrap().is_none());
    assert!(host.install(None, Some(b"archive")).await.0.is_err());
    fs::remove_file(host.0.join("missing-runtime")).unwrap();
    fs::write(host.0.join("manager"), vec![b'!'; helper().len()]).unwrap();
    let error = host.install(None, None).await.0.unwrap_err();
    assert!(error.to_string().contains("checksum"));
  }

  #[tokio::test]
  async fn cached_bytes_cross_ssh_only_for_the_requested_artifact() {
    use PublishedInstallStage as Stage;
    use sha2::{Digest as _, Sha256};

    let host = Host::new();
    let helper = helper();
    let (result, phases) = host.install(Some(helper.as_bytes()), None).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&PublishedInstallStage::UploadingUpdater));
    let total = helper.len() as u64;
    assert!(phases.contains(&Stage::UploadingUpdaterBytes { sent: 0, total }));
    assert!(phases.contains(&Stage::UploadingUpdaterBytes { sent: total, total }));
    assert!(!host.0.join("curl-args").exists());
    assert_eq!(fs::read(host.0.join("received")).unwrap(), MANIFEST);
    let hash = hex::encode(Sha256::digest(&helper));
    let managed = host
      .0
      .join(".tokn/ctl/ctlup/store/updater")
      .join(hash)
      .join("ctlup");
    fs::create_dir_all(managed.parent().unwrap()).unwrap();
    fs::write(managed, &helper).unwrap();
    fs::write(host.0.join("reused"), b"").unwrap();
    let (result, phases) = host.install(Some(helper.as_bytes()), None).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&PublishedInstallStage::UsingInstalledUpdater));
    assert!(phases.contains(&PublishedInstallStage::ReusedRuntime));
    assert!(!phases.contains(&PublishedInstallStage::UploadingUpdater));
    assert!(
      !phases
        .iter()
        .any(|stage| matches!(stage, Stage::UploadingUpdaterBytes { .. }))
    );
    let bytes = b"\0binary\xffarchive";
    let (result, phases) = host.install(None, Some(bytes)).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&PublishedInstallStage::UploadingRuntime));
    let total = bytes.len() as u64;
    assert!(phases.contains(&Stage::UploadingRuntimeBytes { sent: 0, total }));
    assert!(phases.contains(&Stage::UploadingRuntimeBytes { sent: total, total }));
    assert!(
      fs::read_to_string(host.0.join("arguments"))
        .unwrap()
        .ends_with("--archive-stdin\n")
    );
    assert_eq!(
      fs::read(host.0.join("received")).unwrap(),
      [MANIFEST, bytes].concat()
    );
  }

  #[tokio::test]
  async fn retained_updater_export_verifies_bytes_without_reading_ssh_input() {
    use PublishedInstallStage as Stage;
    use std::os::unix::fs::symlink;

    let host = Host::new();
    let helper = helper();
    let hash = hex::encode(Sha256::digest(&helper));
    let bin = host.0.join(".tokn/ctl/bin");
    fs::create_dir_all(&bin).unwrap();
    let launcher = bin.join(".ctl-launcher");
    fs::write(
      &launcher,
      r#"#!/bin/sh
set -eu
[ "$1" = export-manager ]
if [ "$2" = --help ]; then
  if [ -f "$HOME/legacy-manager" ]; then
    echo "error: unrecognized subcommand 'export-manager'" >&2
    exit 2
  fi
  exit 0
fi
[ "$2" = --sha256 ]
printf '%s\n' "$3" > "$HOME/exported-digest"
if IFS= read -r ignored; then exit 1; fi
if [ -f "$HOME/absent-updater" ]; then exit 10; fi
cat "$HOME/.tokn/ctl/ctlup/store/updater/dev-20261121T081525Z-v0.1.4-abcdef0-local/ctlup"
"#,
    )
    .unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(".ctl-launcher", bin.join("ctlup")).unwrap();
    let managed = host
      .0
      .join(".tokn/ctl/ctlup/store/updater/dev-20261121T081525Z-v0.1.4-abcdef0-local")
      .join("ctlup");
    fs::create_dir_all(managed.parent().unwrap()).unwrap();
    fs::write(&managed, &helper).unwrap();

    let (result, phases) = host.install(None, None).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&Stage::UsingInstalledUpdater));
    assert!(!host.0.join("curl-args").exists());
    assert_eq!(
      fs::read_to_string(host.0.join("exported-digest")).unwrap(),
      format!("{hash}\n")
    );
    assert_eq!(fs::read(host.0.join("received")).unwrap(), MANIFEST);

    fs::write(managed, vec![b'!'; helper.len()]).unwrap();
    let error = host
      .install(Some(helper.as_bytes()), None)
      .await
      .0
      .unwrap_err();
    assert!(error.to_string().contains("checksum mismatch"));

    let legacy = host
      .0
      .join(".tokn/ctl/ctlup/store/updater")
      .join(hash)
      .join("ctlup");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, b"corrupt legacy updater").unwrap();
    fs::write(host.0.join("absent-updater"), b"").unwrap();
    let (result, phases) = host.install(Some(helper.as_bytes()), None).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&Stage::UploadingUpdater));

    fs::write(legacy, helper).unwrap();
    fs::write(host.0.join("legacy-manager"), b"").unwrap();
    let (result, phases) = host.install(None, None).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&Stage::UsingInstalledUpdater));
  }

  #[tokio::test]
  async fn missing_legacy_updater_allows_verified_published_recovery() {
    use PublishedInstallStage as Stage;
    use std::os::unix::fs::symlink;

    let host = Host::new();
    let bin = host.0.join(".tokn/ctl/bin");
    fs::create_dir_all(&bin).unwrap();
    let launcher = bin.join(".ctl-launcher");
    let set_probe_error = |message| {
      fs::write(
        &launcher,
        format!("#!/bin/sh\n[ \"$1:$2\" = export-manager:--help ] || exit 2\nprintf '%s\\n' '{message}' >&2\nexit 1\n"),
      )
      .unwrap();
    };
    set_probe_error("ctlup: No such file or directory (os error 2)");
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(".ctl-launcher", bin.join("ctlup")).unwrap();

    let helper = helper();
    let (result, phases) = host.install(Some(helper.as_bytes()), None).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&Stage::UploadingUpdater));
    let (result, phases) = host.install(None, None).await;
    assert!(result.unwrap().is_some());
    assert!(phases.contains(&Stage::DownloadingUpdater));

    set_probe_error("ctlup: stored updater failed payload verification");
    let error = host.install(None, None).await.0.unwrap_err();
    assert!(error.to_string().contains("export capability check failed"));
  }

  #[tokio::test]
  async fn published_lookup_refuses_foreign_updater_entry_points() {
    use std::os::unix::fs::symlink;

    let host = Host::new();
    let bin = host.0.join(".tokn/ctl/bin");
    fs::create_dir_all(&bin).unwrap();
    let outside = host.0.join("foreign-manager");
    fs::write(&outside, "#!/bin/sh\ntouch \"$HOME/foreign-ran\"\n").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(outside, bin.join("ctlup")).unwrap();
    let error = host.install(None, None).await.0.unwrap_err();
    assert!(error.to_string().contains("unexpected link target"));
    assert!(!host.0.join("foreign-ran").exists());
  }
}

/// The Windows OpenSSH smoke fixture installs the supplied native publication
/// first and owns the account for the duration of this test.
#[cfg(windows)]
#[tokio::test]
#[ignore = "requires the isolated Windows OpenSSH CI account"]
async fn windows_openssh_streams_each_cached_publication_artifact() {
  use crate::remote_bundle::{BundleSet, WINDOWS_TARGET, published};
  use ctl_core::installations::CatalogOrigin;
  use sha2::{Digest as _, Sha256};
  use std::{env, fs, path::PathBuf, sync::Mutex};

  let directory = PathBuf::from(env::var_os("CTL_TEST_WINDOWS_DESKTOP_BUNDLE").unwrap());
  let destination = env::var("CTL_TEST_SSH_HOST").unwrap();
  let account = env::var("CTL_TEST_WINDOWS_ACCOUNT_ID").unwrap();
  let manifest = fs::read(directory.join("bundle-set-windows.json")).unwrap();
  let mut updater = published::read_updater(&directory, WINDOWS_TARGET).unwrap();
  let set = BundleSet::parse(&manifest, &updater.info.app_version).unwrap();
  let archive_name = set.archive_name(WINDOWS_TARGET).unwrap();
  let archive = fs::read(directory.join(archive_name)).unwrap();
  let base = format!("{}{}/", published::ROOT, set.bundle_id);
  let origin = crate::installation::encode_catalog_origin(&CatalogOrigin {
    channel: "nightly".into(),
    publication_id: Some(set.bundle_id.clone()),
    built_at: None,
  })
  .unwrap();
  let options = SshConnectionOptions {
    remote_platform: RemotePlatform::Windows,
    ..Default::default()
  };
  for supply_updater in [false, true] {
    if supply_updater {
      // Different digest, same native PE executable. The first run seeded the
      // target's verified runtime cache, so the fixture needs no public network.
      updater
        .bytes
        .extend_from_slice(b"publication bootstrap smoke\n");
    }
    let args: [&str; 9] = [
      &updater.bytes.len().to_string(),
      &hex::encode(Sha256::digest(&updater.bytes)),
      &format!("{base}{}", updater.info.binary),
      &origin,
      &format!("{base}{archive_name}"),
      WINDOWS_TARGET,
      &manifest.len().to_string(),
      if supply_updater { "1" } else { "0" },
      if supply_updater { "0" } else { "1" },
    ];
    let script = script(RemotePlatform::Windows, Some(&account), &args).unwrap();
    let command =
      windows::windows_ssh_command(&destination, &options, &SshInteraction::Batch, &script)
        .await
        .unwrap();
    let phases = Mutex::new(Vec::new());
    let installed = tokio::time::timeout(
      Duration::from_mins(5),
      run(
        command,
        &manifest,
        supply_updater.then_some(updater.bytes.as_slice()),
        (!supply_updater).then_some(archive.as_slice()),
        |phase| phases.lock().unwrap().push(phase),
      ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    installed.validate().unwrap();
    assert_eq!(installed.distribution_id.as_ref(), Some(&set.bundle_id));
    let phases = phases.into_inner().unwrap();
    assert!(phases.contains(&if supply_updater {
      PublishedInstallStage::UploadingUpdater
    } else {
      PublishedInstallStage::UsingInstalledUpdater
    }));
    assert!(phases.contains(&if supply_updater {
      PublishedInstallStage::InstallingRuntime
    } else {
      PublishedInstallStage::UploadingRuntime
    }));
  }
}
