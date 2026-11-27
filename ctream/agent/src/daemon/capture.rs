//! Frame transport on a dedicated daemon connection. Control/event streams
//! never carry pixel data. Encoding and delivery are independent per viewer.
use super::{Opening, endpoint, runtime, write_json};
use crate::capture::{self, pool::Pool};
use ctream_driver::{CaptureLifetime, CaptureSource, Result};
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

async fn frames(source: SourceKey, output: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
  let mut frames = match subscribe(source) {
    Ok(frames) => frames,
    Err(error) => return send_error(output, error).await,
  };
  loop {
    let frame = match frames.next().await {
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
) -> io::Result<()> {
  let mut eof = [0; 1];
  tokio::select! {
    _ = input.read(&mut eof) => Ok(()),
    result = frames(source, &mut output) => result,
  }
}

struct ClientSource {
  source: SourceKey,
  format: CaptureFormat,
}

impl CaptureSource for ClientSource {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    runtime()?.block_on(async {
      let receive = async {
        let mut stream = endpoint::connect().await?;
        write_json(
          &mut stream,
          &Opening::Capture {
            source: self.source,
          },
        )
        .await?;
        let mut header = [0_u8; 8];
        let mut buffer = vec![0_u8; 64 * 1024];
        while stream.read(&mut header[..1]).await? != 0 {
          stream.read_exact(&mut header[1..]).await?;
          let first = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
          let second = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
          if first == 0 {
            if second > ERROR_LIMIT {
              return Err(io::Error::other("invalid capture error"));
            }
            stream.read_exact(&mut buffer[..second]).await?;
            return Err(io::Error::other(
              String::from_utf8_lossy(&buffer[..second]).into_owned(),
            ));
          }
          let size = [first, second];
          let mut remaining = ctream_proto::video::pixel_count(size)? * 3;
          match self.format {
            CaptureFormat::Ppm => write!(output, "P6\n{first} {second}\n255\n")?,
            CaptureFormat::FixedRgb { size: expected } if size != expected => {
              return Err(io::Error::other("unexpected fixed frame dimensions"));
            }
            CaptureFormat::FixedRgb { .. } => {}
          }
          while remaining > 0 {
            let wanted = remaining.min(buffer.len());
            let n = stream.read(&mut buffer[..wanted]).await?;
            if n == 0 {
              return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete capture frame",
              ));
            }
            output.write_all(&buffer[..n])?;
            remaining -= n;
          }
          output.flush()?;
        }
        Ok(())
      };
      let cancelled = async {
        let cancellation = lifetime.cancellation();
        while !cancellation.is_cancelled() {
          tokio::time::sleep(Duration::from_millis(50)).await;
        }
      };
      tokio::select! { result = receive => result, () = cancelled => Ok(()) }
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
