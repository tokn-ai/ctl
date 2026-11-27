use super::*;
use std::{collections::BTreeSet, fs};

/// Count only native capture processes for the fixture display. Decoder and
/// encoder processes use FFmpeg too but never open an x11grab device.
fn capture_processes(display: &str) -> BTreeSet<u32> {
  fs::read_dir("/proc")
    .unwrap()
    .flatten()
    .filter_map(|entry| {
      let pid = entry.file_name().to_str()?.parse().ok()?;
      let args = fs::read(entry.path().join("cmdline")).ok()?;
      let args: Vec<_> = args.split(|b| *b == 0).collect();
      (args
        .windows(2)
        .any(|pair| pair == [b"-f".as_slice(), b"x11grab"])
        && args
          .windows(2)
          .any(|pair| pair == [b"-i".as_slice(), display.as_bytes()]))
      .then_some(pid)
    })
    .collect()
}

#[test]
#[ignore = "requires disposable X11 and FFmpeg"]
fn independent_viewers_use_one_daemon_capture_and_last_disconnect_stops_it()
-> Result<(), Box<dyn Error>> {
  let display = env::var("CTREAM_TEST_X11")?;
  let target = Target::parse(&display)?;
  let client = || {
    Client::new(Local {
      agent: PathBuf::from(env!("CARGO_BIN_EXE_ctream-agent")),
    })
  };
  let mut first = client().session(target, None).capture()?;
  let mut frame = Frame::default();
  first.read_frame(&mut frame)?;
  let workers = capture_processes(&display);
  assert_eq!(workers.len(), 1);
  let mut second = client().session(target, None).capture_with_driver("auto")?;
  second.read_frame(&mut frame)?;
  assert_eq!(capture_processes(&display), workers);
  drop(first);
  second.read_frame(&mut frame)?;
  assert_eq!(capture_processes(&display), workers);
  drop(second);
  wait_for("last capture subscriber releases the native worker", || {
    Ok(capture_processes(&display).is_empty().then_some(()))
  })?;
  Ok(())
}
