use crate::transport::{Cancellation, Channel, after_dispatch};
use std::{
  io::{self, BufRead, BufReader, Read, Write},
  sync::mpsc,
  thread,
  time::Duration,
};

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
  channel: Option<Channel<BufReader<R>, W>>,
  dimensions: Option<[u32; 2]>,
  viewer: Option<ViewerProtocol>,
  last_action_id: Option<String>,
  turn: bool,
  batch: Option<Vec<String>>,
  awaiting_ack: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewerProtocol {
  Turns,
  Daemon,
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Input<R, W> {
  pub fn open(channel: Channel<R, W>, format: InputFormat) -> io::Result<Self> {
    let mut channel = Channel {
      reader: BufReader::new(channel.reader),
      writer: channel.writer,
      cancellation: channel.cancellation,
    };
    let dimensions = Self::read_handshake(&mut channel.reader, format);
    match dimensions {
      Ok(dimensions) => Ok(Self {
        channel: Some(channel),
        dimensions,
        viewer: None,
        last_action_id: None,
        turn: false,
        batch: None,
        awaiting_ack: false,
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

  fn read_handshake(
    reader: &mut impl BufRead,
    format: InputFormat,
  ) -> io::Result<Option<[u32; 2]>> {
    let mut header = String::new();
    reader.take(128).read_line(&mut header)?;
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

  /// Forward an already validated batch to a synchronized platform worker.
  pub fn send_native_batch(&mut self, commands: &[String]) -> io::Result<()> {
    let mut batch = commands.concat();
    batch.push_str("sync\n");
    let mut remaining = batch.as_bytes();
    while !remaining.is_empty() {
      let result = self
        .channel
        .as_mut()
        .expect("open input channel")
        .writer
        .write(remaining);
      let written = match result {
        Ok(0) => return self.check_delivery(Err(io::ErrorKind::WriteZero.into())),
        Ok(written) => written,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
        Err(error) => return self.check_delivery(Err(error)),
      };
      self.awaiting_ack |= !commands.is_empty() && remaining[..written].contains(&b'\n');
      remaining = &remaining[written..];
    }
    let flushed = self
      .channel
      .as_mut()
      .expect("open input channel")
      .writer
      .flush();
    self.check_delivery(flushed)?;
    self.read_ack()
  }

  fn send(&mut self, message: std::fmt::Arguments<'_>) -> io::Result<()> {
    if self.viewer.is_some() {
      let command = message.to_string();
      if let Some(batch) = &mut self.batch {
        if batch.len() >= 256 {
          return Err(io::Error::other("viewer input batch is too large"));
        }
        batch.push(command);
        return Ok(());
      }
      return self.submit(&[command]);
    }
    self.write_command(message, true)
  }

  fn write_command(&mut self, message: std::fmt::Arguments<'_>, mutation: bool) -> io::Result<()> {
    let writer = &mut self.channel.as_mut().expect("open input channel").writer;
    let result = writer.write_fmt(message).and_then(|()| {
      // A newline completes a command. Later writes, including the sync fence,
      // can fail after the backend has already delivered this one.
      self.awaiting_ack |= mutation;
      writer.flush()
    });
    self.check_delivery(result)
  }

  fn check_delivery<U>(&self, result: io::Result<U>) -> io::Result<U> {
    if self.awaiting_ack {
      result.map_err(after_dispatch)
    } else {
      result
    }
  }

  /// Keep the position, modifiers and edges of one viewer event under one admission.
  pub fn begin_event(&mut self) {
    if self.viewer.is_some() {
      self.batch = Some(Vec::new());
    }
  }

  pub fn end_event(&mut self) -> io::Result<()> {
    match self.batch.take() {
      Some(commands) if !commands.is_empty() => self.submit(&commands),
      _ => Ok(()),
    }
  }

  /// Construct a local viewer input transport with the same acknowledged
  /// protocol as a remote viewer (used by the macOS system picker).
  pub fn open_viewer(channel: Channel<R, W>, format: InputFormat) -> io::Result<Self> {
    let mut input = Self::open(channel, format)?;
    input.viewer = Some(ViewerProtocol::Daemon);
    Ok(input)
  }

  pub fn turn_state(&self) -> Option<bool> {
    self.viewer.map(|_| self.turn)
  }

  pub fn host_publishes_actions(&self) -> bool {
    self.viewer == Some(ViewerProtocol::Daemon)
  }

  /// Host journal ID for the last mutation acknowledged on a daemon viewer channel.
  pub fn last_action_id(&self) -> Option<&str> {
    self.last_action_id.as_deref()
  }

  pub fn toggle_turn(&mut self) -> io::Result<()> {
    if self.viewer.is_none() {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "input turns are unavailable",
      ));
    }
    let taking = !self.turn;
    let request = if taking {
      format!("take-turn {}\n", ctream_proto::live::DEFAULT_TURN_MS)
    } else {
      "release-turn\n".into()
    };
    let result = self.request(&request, false);
    if result.is_ok()
      || (!taking
        && result.as_ref().is_err_and(|err| {
          err
            .get_ref()
            .and_then(|e| e.downcast_ref::<ctream_proto::live::TurnError>())
            .is_some_and(|e| e.code == "stale_turn")
        }))
    {
      self.turn = taking;
      return Ok(());
    }
    result
  }

  fn submit(&mut self, commands: &[String]) -> io::Result<()> {
    let scope = if self.turn { " turn" } else { "" };
    let mut packet = format!("batch{scope} {}\n", commands.len());
    for cmd in commands {
      packet.push_str(cmd);
    }
    self.last_action_id = None;
    self.request(&packet, true)
  }

  fn request(&mut self, packet: &str, mutation: bool) -> io::Result<()> {
    let receipt = mutation && self.host_publishes_actions();
    let channel = self.channel.as_mut().expect("open input channel");
    channel.writer.write_all(packet.as_bytes())?;
    channel.writer.flush()?;
    let mut reply = String::new();
    if channel.reader.by_ref().take(2049).read_line(&mut reply)? == 0 {
      return Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "viewer input closed before acknowledgement",
      ));
    }
    if !receipt && reply == "ok\n" {
      return Ok(());
    }
    if receipt && let Some(id) = reply.strip_prefix("ok ").and_then(|s| s.strip_suffix('\n')) {
      self.last_action_id = Some(parse_action_id(Some(id))?);
      return Ok(());
    }
    if reply.ends_with('\n') && reply.len() <= 2048 {
      let response: serde_json::Value = serde_json::from_str(&reply)?;
      let code = response["error"]["code"].as_str();
      let error = match code {
        Some(code @ ("busy" | "stale_turn")) => Some(io::Error::new(
          io::ErrorKind::WouldBlock,
          ctream_proto::live::TurnError {
            code: code.into(),
            owner_id: response["error"]["owner_id"].as_str().map(str::to_owned),
          },
        )),
        Some(code @ ("unknown" | "failed")) if receipt => {
          let message = response["error"]["message"]
            .as_str()
            .unwrap_or("native input failed");
          let error = io::Error::other(message.to_owned());
          Some(if code == "unknown" {
            ctream_driver::UnconfirmedDelivery::error(error)
          } else {
            error
          })
        }
        _ => None,
      };
      if let Some(error) = error {
        if receipt {
          self.last_action_id = Some(parse_action_id(response["action_id"].as_str())?);
        }
        return Err(error);
      }
    }
    Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid viewer input acknowledgement",
    ))
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

  pub fn cancellation(&self) -> Cancellation {
    self
      .channel
      .as_ref()
      .expect("open input channel")
      .cancellation
      .clone()
  }

  pub(crate) fn synchronize(&mut self) -> io::Result<()> {
    self.write_command(format_args!("sync\n"), false)?;
    self.read_ack()
  }

  fn read_ack(&mut self) -> io::Result<()> {
    let mut response = [0; 3];
    let read = self
      .channel
      .as_mut()
      .expect("open input channel")
      .reader
      .read_exact(&mut response);
    self.check_delivery(read)?;
    if &response != b"ok\n" {
      return self.check_delivery(Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid input acknowledgement",
      )));
    }
    self.awaiting_ack = false;
    Ok(())
  }

  pub fn reset(&mut self) -> io::Result<()> {
    self.send(format_args!("reset\n"))
  }

  /// Close a synchronized driver and bound the grace period for its EOF cleanup.
  pub fn finish_bounded(self) -> io::Result<()> {
    let cancellation = self.cancellation();
    let (done, result) = mpsc::channel();
    thread::Builder::new()
      .name("ctream-input-close".into())
      .spawn(move || {
        let _ = done.send(self.finish());
      })?;
    match result.recv_timeout(Duration::from_secs(5)) {
      Ok(result) => result,
      Err(mpsc::RecvTimeoutError::Disconnected) => {
        Err(io::Error::other("input worker exited unexpectedly"))
      }
      Err(mpsc::RecvTimeoutError::Timeout) => {
        cancellation.cancel();
        Err(io::Error::new(
          io::ErrorKind::TimedOut,
          "input did not close after EOF",
        ))
      }
    }
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
    self.check_delivery(result.map(|_| ()))
  }
}

fn parse_action_id(id: Option<&str>) -> io::Result<String> {
  id.filter(|id| uuid::Uuid::try_parse(id).is_ok_and(|value| value.to_string() == *id))
    .map(str::to_owned)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid viewer action receipt"))
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Drop for Input<R, W> {
  fn drop(&mut self) {
    if let Some(channel) = self.channel.take() {
      drop(channel.writer);
      channel.cancellation.cancel();
    }
  }
}

impl<T: crate::transport::Transport> crate::Session<T> {
  /// Negotiate per-event input acknowledgements. An older agent may explicitly
  /// reject the new request before any input has been delivered.
  pub fn viewer_input(&self, label: &str) -> io::Result<Input<T::Reader, T::Writer>> {
    self.viewer_input_with_lease(label, None)
  }

  pub fn viewer_input_with_lease(
    &self,
    label: &str,
    lease: Option<&str>,
  ) -> io::Result<Input<T::Reader, T::Writer>> {
    self.viewer_control(label, lease, None)
  }

  /// Open the physical input lane for an existing viewer, or independently on retained hosts.
  pub fn viewer_control(
    &self,
    label: &str,
    lease: Option<&str>,
    attachment: Option<&crate::live::ViewerAttachment>,
  ) -> io::Result<Input<T::Reader, T::Writer>> {
    self.validate_target()?;
    if lease.is_some_and(|value| {
      self.target != ctream_proto::Target::MacosDesktop || !ctream_proto::valid_capture_lease(value)
    }) {
      return Err(io::Error::other("invalid macOS capture lease"));
    }
    if !ctream_proto::valid_agent_id(label) || !self.supports_input() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid viewer input source or label",
      ));
    }
    let mut arguments = vec![
      ctream_proto::DAEMON_VERSION.into(),
      if attachment.is_some() {
        "viewer-control"
      } else {
        "viewer-input"
      }
      .into(),
      self.target.to_string(),
    ];
    if attachment.is_none() {
      arguments.push(label.into());
    }
    if let Some(window) = &self.window {
      arguments.push(window.id.clone());
    }
    if let Some(lease) = lease {
      arguments.push(lease.to_owned());
    }
    let format = match self.target {
      ctream_proto::Target::X11(_) if self.window.is_some() => InputFormat::X11Window,
      ctream_proto::Target::X11(_) => InputFormat::X11,
      ctream_proto::Target::Wayland(_) => InputFormat::Wayland,
      ctream_proto::Target::Windows(_) | ctream_proto::Target::WindowsDesktop => {
        InputFormat::Windows
      }
      ctream_proto::Target::MacosDesktop => InputFormat::Macos,
    };
    let open = |arguments: &[String], protocol: ViewerProtocol| {
      let mut input = self
        .client
        .transport
        .open(arguments)
        .and_then(|mut channel| {
          if let Some(proof) = attachment {
            proof.write_to(&mut channel.writer)?;
          }
          Input::open(channel, format)
        })?;
      input.viewer = Some(protocol);
      Ok(input)
    };
    match open(&arguments, ViewerProtocol::Daemon) {
      Err(error)
        if attachment.is_none()
          && lease.is_none()
          && crate::transport::unsupported_operation(&error) =>
      {
        arguments[0] = ctream_proto::TURN_VERSION.into();
        match open(&arguments, ViewerProtocol::Turns) {
          Err(error) if crate::transport::unsupported_operation(&error) => self.input(),
          result => result,
        }
      }
      result => result,
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
  fn viewer_turn_packets_keep_the_scope_until_explicit_release_and_never_replay() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let ids: Vec<_> = (0..4).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    let result = format!(
      "ready\nok\n{}\n{}\nok {}\n{}\nok {}\n",
      serde_json::json!({"error":{"code":"stale_turn"},"action_id":ids[0]}),
      serde_json::json!({"error":{"code":"stale_turn"}}),
      ids[1],
      serde_json::json!({"error":{"code":"busy","owner_id":"other"},"action_id":ids[2]}),
      ids[3]
    );
    let mut input = Input::open_viewer(
      Channel {
        reader: Cursor::new(result),
        writer: Writer(observed.clone()),
        cancellation: Cancellation::default(),
      },
      InputFormat::Macos,
    )
    .unwrap();
    input.toggle_turn().unwrap();
    input.begin_event();
    input.move_pointer(2, 3, [8, 6]).unwrap();
    input.button(1, true).unwrap();
    assert_eq!(
      input.end_event().unwrap_err().kind(),
      io::ErrorKind::WouldBlock
    );
    assert_eq!(input.turn_state(), Some(true));
    input.toggle_turn().unwrap();
    assert_eq!(input.turn_state(), Some(false));
    input.move_pointer(4, 1, [8, 6]).unwrap();
    let busy = input.move_pointer(1, 1, [8, 6]).unwrap_err();
    assert!(busy.to_string().contains("other"));
    assert_eq!(input.last_action_id(), Some(ids[2].as_str()));
    input.move_pointer(5, 5, [8, 6]).unwrap();
    assert_eq!(input.last_action_id(), Some(ids[3].as_str()));
    assert_eq!(&*observed.lock().unwrap(),b"take-turn 10000\nbatch turn 2\nmove 2 3 8 6\nbutton 1 1\nrelease-turn\nbatch 1\nmove 4 1 8 6\nbatch 1\nmove 1 1 8 6\nbatch 1\nmove 5 5 8 6\n");
  }

  #[test]
  fn native_receipt_distinguishes_driver_failure_from_lost_delivery() {
    let id = uuid::Uuid::new_v4().to_string();
    let mut stream = String::from("ready\n");
    for code in ["failed", "unknown"] {
      stream.push_str(
        &serde_json::json!({
          "action_id":id,"error":{"code":code,"message":"driver outcome"}
        })
        .to_string(),
      );
      stream.push('\n');
    }
    stream.push_str("ok\n");
    let mut input = Input::open_viewer(
      Channel {
        reader: Cursor::new(stream.into_bytes()),
        writer: io::sink(),
        cancellation: Cancellation::default(),
      },
      InputFormat::X11Window,
    )
    .unwrap();
    for uncertain in [false, true] {
      let error = input.key("a", true).unwrap_err();
      assert_eq!(ctream_driver::UnconfirmedDelivery::is(&error), uncertain);
      assert_eq!(input.last_action_id(), Some(id.as_str()));
    }
    assert_eq!(
      input.key("a", true).unwrap_err().kind(),
      io::ErrorKind::InvalidData
    );
    assert!(input.last_action_id().is_none());
  }

  #[test]
  fn synchronization_consumes_one_acknowledgement_per_request() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let mut input = Input::open(
      Channel {
        reader: Cursor::new(b"ready\nok\nok\nbad"),
        writer: Writer(Arc::clone(&bytes)),
        cancellation: Cancellation::default(),
      },
      InputFormat::X11Window,
    )
    .unwrap();
    input.synchronize().unwrap();
    input.synchronize().unwrap();
    assert_eq!(
      input.synchronize().unwrap_err().kind(),
      io::ErrorKind::InvalidData
    );
    assert_eq!(
      input.synchronize().unwrap_err().kind(),
      io::ErrorKind::UnexpectedEof
    );
    assert_eq!(*bytes.lock().unwrap(), b"sync\nsync\nsync\nsync\n");
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

#[cfg(test)]
mod negotiation_tests {
  use super::*;
  use crate::{Client, transport::Transport};
  use ctream_proto::{DAEMON_VERSION, TURN_VERSION, Target};
  use std::{
    io::{Cursor, Sink},
    sync::{Arc, Mutex},
  };

  struct Host {
    supported: &'static str,
    opened: Arc<Mutex<Vec<String>>>,
  }
  impl Transport for Host {
    type Reader = Cursor<Vec<u8>>;
    type Writer = Sink;
    fn open(&self, args: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
      self.opened.lock().unwrap().push(args[0].clone());
      if args[0] != self.supported {
        return Err(io::Error::other(
          "ctream-agent: usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]",
        ));
      }
      Ok(Channel {
        reader: Cursor::new(b"800 600\n".to_vec()),
        writer: io::sink(),
        cancellation: Cancellation::default(),
      })
    }
  }

  #[test]
  fn only_daemon_viewers_rely_on_host_published_actions() {
    for (supported, expected) in [
      (DAEMON_VERSION, vec![DAEMON_VERSION]),
      (TURN_VERSION, vec![DAEMON_VERSION, TURN_VERSION]),
    ] {
      let opened = Arc::default();
      let client = Client::new(Host {
        supported,
        opened: Arc::clone(&opened),
      });
      let input = client
        .session(Target::X11(0), None)
        .viewer_input("alice")
        .unwrap();
      assert_eq!(input.turn_state(), Some(false));
      assert_eq!(input.host_publishes_actions(), supported == DAEMON_VERSION);
      assert_eq!(*opened.lock().unwrap(), expected);
    }
  }
}
