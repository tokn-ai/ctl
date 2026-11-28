//! Frame transport on a dedicated daemon connection. Control/event streams
//! never carry pixel data. Encoding and delivery are independent per viewer.
use super::server::lane::{Gate, Phase};
use super::{Opening, runtime, transport, write_json};
use crate::capture::{
  self,
  pool::{Frames, Pool},
};
use ctream_driver::{CaptureLifetime, CaptureSource, Frame, Result};
use ctream_proto::{CaptureFormat, Target, VideoRequest, valid_window_id};
use serde::{Deserialize, Serialize};
use std::{
  io::{self, Write},
  sync::LazyLock,
  time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Clone, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceKey {
  target: String,
  driver: String,
  window: Option<String>,
  fps: Option<u16>,
}

impl SourceKey {
  pub(in crate::daemon) fn validate(&self) -> io::Result<()> {
    self.selection().map(drop)
  }

  fn selection(&self) -> io::Result<(Target, capture::Selection)> {
    let invalid = |msg| io::Error::new(io::ErrorKind::InvalidInput, msg);
    let target = Target::parse(&self.target).map_err(invalid)?;
    if self
      .window
      .as_deref()
      .is_some_and(|id| !valid_window_id(id))
      || self.fps.is_some_and(|fps| !(1..=120).contains(&fps))
      || target.to_string() != self.target
    {
      return Err(invalid("invalid capture source"));
    }
    let selected = capture::select(target, &self.driver, self.window.is_some())
      .map_err(|e| io::Error::other(e.to_string()))?;
    if selected.id != self.driver {
      return Err(invalid("expected a resolved capture driver"));
    }
    Ok((target, selected))
  }
}

static SOURCES: LazyLock<Pool<SourceKey>> = LazyLock::new(Pool::default);

const ERROR_LIMIT: usize = 4096;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

async fn send_error(output: &mut (impl AsyncWrite + Unpin), error: io::Error) -> io::Result<()> {
  let message = error.to_string();
  let bytes = &message.as_bytes()[..message.len().min(ERROR_LIMIT)];
  tokio::time::timeout(WRITE_TIMEOUT, async {
    output.write_u32_le(0).await?;
    output.write_u32_le(bytes.len() as u32).await?;
    output.write_all(bytes).await
  })
  .await?
}

fn subscribe(source: SourceKey) -> io::Result<capture::pool::Subscription> {
  let (target, selected) = source.selection()?;
  let mut key = source.clone();
  // Some native providers have a fixed cadence, while others use the same
  // concrete default for both raw capture and explicitly requested video.
  match selected.id {
    "linux.wayland.image_copy" => key.fps = None,
    "linux.x11.ffmpeg" | "linux.x11.composite" | "windows.dxgi" | "windows.wgc" => {
      key.fps = Some(
        source
          .fps
          .unwrap_or_else(|| ctream_driver::CaptureOptions::default().max_fps()),
      )
    }
    _ => {}
  }
  SOURCES.subscribe(key, move |frames, lifetime| {
    capture::platform::capture(
      target,
      source.window.as_deref(),
      selected,
      source.fps,
      frames,
      lifetime,
    )
  })
}

async fn frames(
  source: SourceKey,
  output: &mut (impl AsyncWrite + Unpin),
  gate: &Gate,
) -> io::Result<()> {
  let mut frames = match subscribe(source) {
    Ok(frames) => frames,
    Err(error) => return send_error(output, error).await,
  };
  loop {
    let frame = match tokio::select! {
      biased;
      _ = gate.next_output_barrier() => {
        // Switch only before a new frame. Once its dimensions are on the wire
        // the continuation must supply every declared pixel to the encoder.
        if gate.park(Phase::Freeze, Phase::Freeze, None).await { return Ok(()); }
        continue;
      }
      next = frames.next() => next,
    } {
      Ok(Some(frame)) => frame,
      Ok(None) => return Ok(()),
      Err(error) => return send_error(output, error).await,
    };
    // A failed partial frame must terminate the channel. Sending an error
    // record here would be indistinguishable from the remaining pixel bytes.
    tokio::time::timeout(WRITE_TIMEOUT, async {
      output.write_u32_le(frame.size[0] as u32).await?;
      output.write_u32_le(frame.size[1] as u32).await?;
      output.write_all(&frame.pixels).await
    })
    .await??;
  }
}

pub(super) async fn serve(
  source: SourceKey,
  mut input: impl AsyncRead + Unpin,
  mut output: impl AsyncWrite + Unpin,
  gate: &Gate,
) -> io::Result<()> {
  let mut eof = [0; 1];
  tokio::select! {
    _ = input.read(&mut eof) => Ok(()),
    result = frames(source, &mut output, gate) => result,
  }
}

struct ClientSource {
  source: SourceKey,
  format: CaptureFormat,
}

async fn receive_frames(
  mut stream: impl AsyncRead + Unpin,
  format: CaptureFormat,
  frames: &Frames,
) -> io::Result<()> {
  let mut header = [0_u8; 8];
  let mut pixels = Vec::new();
  while stream.read(&mut header[..1]).await? != 0 {
    stream.read_exact(&mut header[1..]).await?;
    let width = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    let height = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
    if width == 0 {
      if height > ERROR_LIMIT {
        return Err(io::Error::other("invalid capture error"));
      }
      let mut error = vec![0; height];
      stream.read_exact(&mut error).await?;
      return Err(io::Error::other(
        String::from_utf8_lossy(&error).into_owned(),
      ));
    }
    let size = [width, height];
    let bytes = ctream_proto::video::pixel_count(size)? * 3;
    if let CaptureFormat::FixedRgb { size: expected } = format
      && expected != size
    {
      return Err(io::Error::other("unexpected fixed frame dimensions"));
    }
    pixels
      .try_reserve(bytes.saturating_sub(pixels.len()))
      .map_err(io::Error::other)?;
    pixels.resize(bytes, 0);
    stream.read_exact(&mut pixels).await?;
    pixels = frames.publish(Frame { size, pixels });
  }
  Ok(())
}

impl CaptureSource for ClientSource {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    let cancellation = lifetime.cancellation();
    let format = self.format;
    let (frames, mut latest) = Frames::channel();
    // A stalled stdout/encoder may keep drawing its last frame. Keep receiving
    // on the shared I/O reactor so the private lane can complete a handoff.
    // JoinSet aborts that task when the writer exits; stdin EOF also cancels it
    // independently when a blocking OS write is still in progress.
    let mut worker = tokio::task::JoinSet::new();
    worker.spawn_on(
      async move {
        let receive = async {
          let mut stream = transport::connect(None, false).await?;
          write_json(
            &mut stream,
            &Opening::Capture {
              source: self.source,
            },
          )
          .await?;
          receive_frames(stream, format, &frames).await
        };
        let cancelled = async {
          while !cancellation.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(50)).await;
          }
        };
        tokio::select! { result = receive => result, () = cancelled => Ok(()) }
      },
      transport::background()?.handle(),
    );
    runtime()?.block_on(async {
      while let Some(frame) = latest.next().await? {
        if format == CaptureFormat::Ppm {
          let [width, height] = frame.size;
          write!(output, "P6\n{width} {height}\n255\n")?;
        }
        output.write_all(&frame.pixels)?;
        output.flush()?;
      }
      worker
        .join_next()
        .await
        .expect("one capture receiver")
        .map_err(io::Error::other)?
    })?;
    Ok(())
  }
}

pub fn stream(
  target: Target,
  driver: &str,
  window: Option<&str>,
  video: Option<VideoRequest>,
  metadata: bool,
) -> Result<()> {
  let selected = capture::select(target, driver, window.is_some())?;
  let source = ClientSource {
    source: SourceKey {
      target: target.to_string(),
      driver: selected.id.to_owned(),
      window: window.map(str::to_owned),
      fps: video.as_ref().map(|r| r.options.max_fps),
    },
    format: selected.format,
  };
  if metadata {
    selected.stream(source, video)
  } else {
    crate::runtime::stream_capture(source)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn a_stalled_encoder_can_resume_at_the_latest_complete_frame() {
    let (mut host, input) = tokio::io::duplex(16);
    let (frames, mut viewer) = Frames::channel();
    let receive =
      tokio::spawn(async move { receive_frames(input, CaptureFormat::Ppm, &frames).await });
    async fn send(host: &mut (impl AsyncWrite + Unpin), byte: u8) {
      host.write_u32_le(1).await.unwrap();
      host.write_u32_le(1).await.unwrap();
      host.write_all(&[byte; 3]).await.unwrap();
    }
    send(&mut host, 1).await;
    let displayed = viewer.next().await.unwrap().unwrap();
    send(&mut host, 2).await;
    send(&mut host, 3).await;
    // A torn frame from an ordinary socket loss must never enter the external
    // encoder, even when that encoder is still writing the first frame.
    host.write_u32_le(1).await.unwrap();
    host.write_u32_le(1).await.unwrap();
    host.write_all(&[4]).await.unwrap();
    drop(host);
    assert_eq!(
      receive.await.unwrap().unwrap_err().kind(),
      io::ErrorKind::UnexpectedEof
    );
    assert_eq!(displayed.pixels, [1; 3]);
    assert_eq!(viewer.next().await.unwrap().unwrap().pixels, [3; 3]);
    assert!(viewer.next().await.unwrap().is_none());
  }
}
