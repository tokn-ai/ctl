//! Synthetic user drivers exercise the public API without official backends.

use ctream_driver::{
  CaptureDisplay, CaptureLifetime, CaptureOptions, CaptureSource, Driver, Result,
};
use std::{
  io::Write,
  sync::{Arc, Mutex, mpsc},
  time::Duration,
};

pub struct Device {
  id: String,
  pixel: [u8; 3],
}

// These modules represent capabilities supplied together by a user driver crate.
mod capture {
  use super::*;

  pub struct Driver;
  pub struct Session([u8; 3]);
  pub struct Source([u8; 3]);

  impl ctream_driver::Driver for Driver {
    type Target = Device;
    type Session = Session;

    fn open(&self, target: &Device) -> Result<Session> {
      Ok(Session(target.pixel))
    }
  }

  // This session deliberately has no input, window, or discovery capability.
  impl CaptureDisplay for Session {
    type Source = Source;

    fn capture_display(&self) -> Result<Source> {
      Ok(Source(self.0))
    }
  }

  impl CaptureSource for Source {
    fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
      if !lifetime.cancelled(Duration::ZERO) {
        output.write_all(b"P6\n1 1\n255\n")?;
        output.write_all(&self.0)?;
      }
      Ok(())
    }
  }
}

mod input {
  use super::*;
  use ctream_driver::{ForwardInput, InputSource};

  pub struct Driver(pub Arc<Mutex<Vec<String>>>);
  pub struct Session {
    target: String,
    operations: Arc<Mutex<Vec<String>>>,
  }
  pub struct Source(Session);

  impl ctream_driver::Driver for Driver {
    type Target = str;
    type Session = Session;

    fn open(&self, target: &str) -> Result<Session> {
      Ok(Session {
        target: target.to_owned(),
        operations: Arc::clone(&self.0),
      })
    }
  }

  impl ForwardInput for Session {
    type Input = Source;

    fn input(&self) -> Result<Source> {
      Ok(Source(Session {
        target: self.target.clone(),
        operations: Arc::clone(&self.operations),
      }))
    }
  }

  impl InputSource for Source {
    fn run(self) -> Result<()> {
      self.0.operations.lock().unwrap().push(self.0.target);
      Ok(())
    }
  }
}

fn capture<D>(driver: &D, target: &D::Target, lifetime: &CaptureLifetime) -> Result<Vec<u8>>
where
  D: Driver,
  D::Session: CaptureDisplay,
{
  let mut output = Vec::new();
  driver
    .open(target)?
    // A custom driver implementing the original capability keeps compiling
    // when a host supplies capture options through the provided method.
    .capture_display_with_options(CaptureOptions::new(30)?)?
    .run(&mut output, lifetime)?;
  Ok(output)
}

#[test]
fn capture_only_driver_accepts_a_user_target_and_observes_cancellation() -> Result<()> {
  let device = Device {
    id: "synthetic-device".into(),
    pixel: [4, 5, 6],
  };
  let (sender, receiver) = mpsc::channel();
  let lifetime = CaptureLifetime::from_receiver(receiver);
  assert_eq!(
    capture(&capture::Driver, &device, &lifetime)?,
    b"P6\n1 1\n255\n\x04\x05\x06"
  );
  sender.send(())?;
  assert!(capture(&capture::Driver, &device, &lifetime)?.is_empty());
  // A cancellation stays set even while its sender remains alive.
  assert!(capture(&capture::Driver, &device, &lifetime)?.is_empty());
  drop(sender);
  Ok(())
}

#[test]
fn a_second_capability_can_use_a_borrowed_target_without_capture_bounds() -> Result<()> {
  use ctream_driver::{ForwardInput, InputSource};

  let device = Device {
    id: "synthetic-device".into(),
    pixel: [4, 5, 6],
  };
  let operations = Arc::new(Mutex::new(Vec::new()));
  let driver = input::Driver(Arc::clone(&operations));
  let session = driver.open(&device.id)?;
  drop(device);
  session.input()?.run()?;
  assert_eq!(*operations.lock().unwrap(), ["synthetic-device"]);
  Ok(())
}
