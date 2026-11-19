mod capture;
use capture::Target;
mod input;

use eframe::egui;
use std::{
  env,
  io::{self, BufReader},
  process::{Child, Command, Stdio},
  sync::{Arc, Mutex},
  thread,
};

const WIDTH: usize = 960;
const HEIGHT: usize = 540;

#[derive(Default)]
struct Stream {
  frame: Option<egui::ColorImage>,
  error: Option<String>,
}

fn ssh(host: &str) -> Command {
  let mut command = Command::new("ssh");
  command.args([
    "-C",
    "-T",
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=10",
    "--",
    host,
  ]);
  command
}

struct Preview {
  input: Option<input::Input>,
  forwarding: bool,
  input_error: Option<String>,
  ssh: Child,
  stream: Arc<Mutex<Stream>>,
  texture: Option<egui::TextureHandle>,
  frames: u64,
}

impl Preview {
  fn connect(host: &str, target: Target, ctx: egui::Context) -> io::Result<Self> {
    let mut input_error = None;
    let (input, capture) = match target {
      Target::X11(display) => {
        let input = input::Input::connect(host, &target)?;
        let [w, h] = input.desktop_size().expect("X11 geometry");
        let capture = format!(
          "exec ffmpeg -nostdin -hide_banner -loglevel error -f x11grab -framerate 8 -video_size {w}x{h} -i :{display} \
           -vf 'scale={WIDTH}:{HEIGHT}' -pix_fmt rgb24 -f rawvideo pipe:1"
        );
        (Some(input), capture)
      }
      Target::Wayland(number) => (
        input::Input::connect(host, &target)
          .map_err(|error| {
            input_error = Some(format!("View only ({error})"));
          })
          .ok(),
        format!(
          r#"exec env WAYLAND_DISPLAY=wayland-{number} sh -c 'PATH="$PATH:$HOME/.local/bin"; export PATH; while grim -t ppm -; do sleep 0.125; done'"#
        ),
      ),
    };
    let mut ssh = ssh(host)
      .arg(capture)
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .spawn()?;
    let mut stdout = BufReader::new(ssh.stdout.take().expect("piped SSH output"));
    let stream = Arc::new(Mutex::new(Stream::default()));
    let reader = Arc::clone(&stream);
    thread::spawn(move || {
      let mut buffer = Vec::new();
      loop {
        let image = match capture::read_frame(&mut stdout, &target, &mut buffer) {
          Ok(image) => image,
          Err(error) => {
            reader.lock().unwrap().error = Some(format!(
              "Capture ended ({error}). See terminal for remote diagnostics."
            ));
            ctx.request_repaint();
            break;
          }
        };
        reader.lock().unwrap().frame = Some(image);
        ctx.request_repaint();
      }
    });
    Ok(Self {
      input,
      forwarding: false,
      input_error,
      ssh,
      stream,
      texture: None,
      frames: 0,
    })
  }
}

impl Drop for Preview {
  fn drop(&mut self) {
    let _ = self.ssh.kill();
    let _ = self.ssh.wait();
  }
}

impl eframe::App for Preview {
  fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    let (frame, error) = {
      let mut stream = self.stream.lock().unwrap();
      (stream.frame.take(), stream.error.clone())
    };
    if let Some(image) = frame {
      if let Some(texture) = &mut self.texture {
        texture.set(image, egui::TextureOptions::LINEAR);
      } else {
        self.texture = Some(ctx.load_texture("desktop", image, egui::TextureOptions::LINEAR));
      }
      self.frames += 1;
      if self.frames == 2 {
        eprintln!("Live preview received two frames on the wgpu renderer.");
      }
    }
    egui::CentralPanel::default().show(ctx, |ui| {
      if let Some(error) = error.as_ref().or(self.input_error.as_ref()) {
        ui.label(error);
      } else if self.texture.is_none() {
        ui.spinner();
        ui.label("Waiting for the first frame…");
      }
      if let Some(texture) = &self.texture {
        let size = self
          .input
          .as_ref()
          .and_then(input::Input::desktop_size)
          .unwrap_or_else(|| texture.size().map(|v| v as u32));
        let rect = input::image_rect(ui.available_rect_before_wrap(), size);
        ui.painter().image(
          texture.id(),
          rect,
          egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
          egui::Color32::WHITE,
        );
        if let Some(input) = &mut self.input
          && ctx.current_pass_index() == 0
          && self.input_error.is_none()
        {
          let active = error.is_none() && ctx.input(|i| i.focused);
          let result = if active {
            ctx.input(|i| input.forward(rect, size, &i.raw.events, i.modifiers))
          } else if self.forwarding {
            input.reset()
          } else {
            Ok(())
          };
          self.forwarding = active;
          if let Err(err) = result {
            self.input_error = Some(format!("Input disconnected ({err})"));
          }
        }
      }
    });
  }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<_> = env::args().skip(1).collect();
  let [host, display] = args.as_slice() else {
    return Err("usage: ctream-preview SSH_HOST (:DISPLAY | wayland-N)".into());
  };
  let target = Target::parse(display)?;
  let mode = match target {
    Target::X11(_) => "control",
    Target::Wayland(_) => "preview",
  };
  let title = format!("ctream {mode} · {host} {display}");
  let options = eframe::NativeOptions {
    renderer: eframe::Renderer::Wgpu,
    viewport: egui::ViewportBuilder::default().with_inner_size([WIDTH as f32, HEIGHT as f32]),
    ..Default::default()
  };
  eframe::run_native(
    &title,
    options,
    Box::new(|cc| {
      Ok(Box::new(Preview::connect(
        host,
        target,
        cc.egui_ctx.clone(),
      )?))
    }),
  )?;
  Ok(())
}
