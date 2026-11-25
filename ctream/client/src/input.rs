use crate::transport::Channel;
use std::io::{self, BufRead, BufReader, Read, Write};

#[derive(Clone, Copy)]
pub enum InputFormat {
  X11,
  X11Window,
  Wayland,
  Windows,
  Macos,
}

pub(crate) fn valid_key_name(name: &str) -> bool {
  // "key NAME 1\n" must fit the retained 128-byte input command limit.
  !name.is_empty()
    && name.len() <= 121
    && name
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// An opened input capability. Dropping its writer asks the agent to release held input.
pub struct Input<R: Read + Send + 'static, W: Write + Send + 'static> {
  channel: Option<Channel<R, W>>,
  dimensions: Option<[u32; 2]>,
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Input<R, W> {
  pub(crate) fn open(mut channel: Channel<R, W>, format: InputFormat) -> io::Result<Self> {
    let dimensions = Self::read_handshake(&mut channel.reader, format);
    match dimensions {
      Ok(dimensions) => Ok(Self {
        channel: Some(channel),
        dimensions,
      }),
      Err(error) => {
        let Channel {
          reader,
          writer,
          cancellation,
        } = channel;
        drop(writer);
        cancellation.cancel();
        drop(reader);
        Err(error)
      }
    }
  }

  fn read_handshake(reader: &mut R, format: InputFormat) -> io::Result<Option<[u32; 2]>> {
    let mut header = String::new();
    BufReader::new(reader).take(128).read_line(&mut header)?;
    if !header.ends_with('\n') {
      return Err(io::Error::other("incomplete remote input handshake"));
    }
    match format {
      InputFormat::X11Window | InputFormat::Wayland | InputFormat::Windows | InputFormat::Macos
        if header.trim_end() == "ready" =>
      {
        Ok(None)
      }
      InputFormat::X11 => {
        let dimensions: Vec<_> = header.split_whitespace().map(str::parse::<u32>).collect();
        match dimensions.as_slice() {
          [Ok(width), Ok(height)] if *width > 0 && *height > 0 => Ok(Some([*width, *height])),
          _ => Err(io::Error::other("X11 input initialization failed")),
        }
      }
      _ => Err(io::Error::other("remote input initialization failed")),
    }
  }

  pub fn dimensions(&self) -> Option<[u32; 2]> {
    self.dimensions
  }

  fn send(&mut self, message: std::fmt::Arguments<'_>) -> io::Result<()> {
    let writer = &mut self.channel.as_mut().expect("open input channel").writer;
    writer.write_fmt(message)?;
    writer.flush()
  }

  pub fn move_pointer(&mut self, x: u32, y: u32, size: [u32; 2]) -> io::Result<()> {
    let dimensions = self.dimensions.unwrap_or(size);
    if size.contains(&0) || x >= dimensions[0] || y >= dimensions[1] || x >= size[0] || y >= size[1]
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "pointer is outside capture dimensions",
      ));
    }
    if self.dimensions.is_some() {
      self.send(format_args!("move {x} {y}\n"))
    } else {
      self.send(format_args!("move {x} {y} {} {}\n", size[0], size[1]))
    }
  }

  pub fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
    self.send(format_args!("button {code} {}\n", u8::from(pressed)))
  }

  pub fn key(&mut self, name: &str, pressed: bool) -> io::Result<()> {
    if !valid_key_name(name) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid key name",
      ));
    }
    self.send(format_args!("key {name} {}\n", u8::from(pressed)))
  }

  pub fn reset(&mut self) -> io::Result<()> {
    self.send(format_args!("reset\n"))
  }

  /// Close stdin and wait for the agent to process the queued events and release
  /// held input. Process transports propagate the agent's exit status at EOF.
  pub fn finish(mut self) -> io::Result<()> {
    let Channel {
      mut reader,
      writer,
      cancellation,
    } = self.channel.take().expect("open input channel");
    drop(writer);
    let result = io::copy(&mut reader, &mut io::sink());
    cancellation.cancel();
    result.map(|_| ())
  }
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Drop for Input<R, W> {
  fn drop(&mut self) {
    if let Some(channel) = self.channel.take() {
      drop(channel.writer);
      channel.cancellation.cancel();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::transport::Cancellation;
  use std::{
    io::Cursor,
    sync::{Arc, Mutex},
  };

  struct Writer(Arc<Mutex<Vec<u8>>>);

  impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      self.0.lock().unwrap().extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  #[test]
  fn input_encodes_each_backend_and_rejects_out_of_bounds_or_injected_commands() {
    for (format, header, expected) in [
      (InputFormat::X11, "800 600\n", "move 400 300\n"),
      (InputFormat::X11Window, "ready\n", "move 400 300 800 600\n"),
      (InputFormat::Wayland, "ready\n", "move 400 300 800 600\n"),
      (InputFormat::Windows, "ready\n", "move 400 300 800 600\n"),
    ] {
      let bytes = Arc::new(Mutex::new(Vec::new()));
      let channel = Channel {
        reader: Cursor::new(header.as_bytes().to_vec()),
        writer: Writer(Arc::clone(&bytes)),
        cancellation: Cancellation::default(),
      };
      let mut input = Input::open(channel, format).unwrap();
      input.move_pointer(400, 300, [800, 600]).unwrap();
      assert_eq!(*bytes.lock().unwrap(), expected.as_bytes());
      assert!(input.move_pointer(800, 300, [800, 600]).is_err());
      assert!(input.key("Return\nreset", true).is_err());
      assert!(input.key(&"a".repeat(122), true).is_err());
      if input.dimensions().is_some() {
        assert!(input.move_pointer(1000, 300, [2000, 600]).is_err());
      }
      input.key("Return", true).unwrap();
      input.button(1, false).unwrap();
      input.reset().unwrap();
    }
  }

  #[test]
  fn rejected_handshake_closes_writer_before_requesting_cleanup() {
    struct DropWriter(Arc<Mutex<Vec<&'static str>>>);
    impl Write for DropWriter {
      fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(data.len())
      }
      fn flush(&mut self) -> io::Result<()> {
        Ok(())
      }
    }
    impl Drop for DropWriter {
      fn drop(&mut self) {
        self.0.lock().unwrap().push("eof");
      }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::clone(&events);
    let channel = Channel {
      reader: Cursor::new(b"invalid\n".to_vec()),
      writer: DropWriter(Arc::clone(&events)),
      cancellation: Cancellation::new(move || cancelled.lock().unwrap().push("cancel")),
    };
    assert!(Input::open(channel, InputFormat::X11).is_err());
    assert_eq!(*events.lock().unwrap(), ["eof", "cancel"]);
  }
}
