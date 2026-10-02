#[path = "../build_support.rs"]
mod build_support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
      "ctl-package-metadata-{}-{}",
      std::process::id(),
      NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory).unwrap();
    Self(directory)
  }

  fn write(&self, name: &str, contents: &str) {
    let path = self.0.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
  }

  fn package(&self) -> &Path {
    &self.0
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    std::fs::remove_dir_all(&self.0).unwrap();
  }
}

#[test]
fn isolated_package_uses_archive_provenance_and_ignores_neighbours() {
  let fixture = Fixture::new();
  fixture.write("Cargo.toml", "[workspace]\n");
  fixture.write("Cargo.lock", "# enclosing workspace\n");
  for name in ["ctl", "ctmux", "task", "ctmux-process-info"] {
    fixture.write(&format!("{name}/src/lib.rs"), "// enclosing source\n");
  }
  fixture.write("ctl-core/Cargo.toml", "[package]\nname = 'ctl-core'\n");
  fixture.write("ctl-core/src/lib.rs", "// package source\n");
  fixture.write(
    "ctl-core/.cargo_vcs_info.json",
    &format!("{{\"git\":{{\"sha1\":\"{}\"}}}}", "a".repeat(40)),
  );
  let package = fixture.package().join("ctl-core");
  let before = build_support::read_identity(&package);
  assert_eq!(before.revision.as_deref(), Some("a".repeat(40).as_str()));
  assert!(!before.dirty);
  fixture.write("unrelated/src/lib.rs", "// unrelated enclosing source\n");
  fixture.write("ctmux/src/lib.rs", "// changed enclosing workspace\n");
  let after = build_support::read_identity(&package);
  assert_eq!(before.fingerprint, after.fingerprint);
  fixture.write("ctl-core/src/lib.rs", "// modified package source\n");
  assert_ne!(
    before.fingerprint,
    build_support::read_identity(&package).fingerprint
  );
}

#[test]
fn missing_invalid_and_dirty_provenance_never_claim_a_clean_release() {
  let fixture = Fixture::new();
  fixture.write("Cargo.toml", "[package]\nname = 'ctl-core'\n");
  fixture.write("src/lib.rs", "// source\n");
  for provenance in [
    None,
    Some("not json"),
    Some("{\"git\":{\"sha1\":\"invalid\"}}"),
  ] {
    if let Some(contents) = provenance {
      fixture.write(".cargo_vcs_info.json", contents);
    }
    let identity = build_support::read_identity(fixture.package());
    assert!(identity.revision.is_none());
    assert!(identity.dirty);
  }
  fixture.write(
    ".cargo_vcs_info.json",
    &format!(
      "{{\"git\":{{\"sha1\":\"{}\",\"dirty\":true}}}}",
      "b".repeat(40)
    ),
  );
  let identity = build_support::read_identity(fixture.package());
  assert_eq!(identity.revision, Some("b".repeat(40)));
  assert!(identity.dirty);
}

#[test]
fn embedded_assets_affect_workspace_identity_but_user_files_do_not() {
  let fixture = Fixture::new();
  fixture.write("Cargo.toml", "[workspace]\n");
  fixture.write("Cargo.lock", "# lock\n");
  for name in ["ctl-core", "ctl", "ctmux", "task", "ctmux-process-info"] {
    fixture.write(&format!("{name}/src/lib.rs"), "// source\n");
  }
  fixture.write("ctl/daemon/assets/vpn/heartbeat.sh", "echo heartbeat\n");
  fixture.write("ctl/cli/skills/ctl/SKILL.md", "# bundled skill\n");
  let manifest_dir = fixture.package().join("ctl-core");
  let before = build_support::read_identity(&manifest_dir);
  fixture.write("ctl/daemon/.env", "SECRET=must-not-be-read\n");
  fixture.write(
    "ctl/daemon/target/generated.rs",
    "// ignored build output\n",
  );
  assert_eq!(
    before.fingerprint,
    build_support::read_identity(&manifest_dir).fingerprint
  );
  fixture.write("ctl/daemon/assets/vpn/heartbeat.sh", "echo changed\n");
  let script_changed = build_support::read_identity(&manifest_dir);
  assert_ne!(before.fingerprint, script_changed.fingerprint);
  fixture.write("ctl/cli/skills/ctl/SKILL.md", "# changed skill\n");
  assert_ne!(
    script_changed.fingerprint,
    build_support::read_identity(&manifest_dir).fingerprint
  );
  let before_paths = build_support::read_identity(&manifest_dir);
  fixture.write("ctl-core/src/paths.rs", "// changed storage root\n");
  assert_ne!(
    before_paths.fingerprint,
    build_support::read_identity(&manifest_dir).fingerprint
  );
  assert!(before.revision.is_none());
  assert!(before.dirty);
}
