use eframe::egui::{self, FontData, FontDefinitions};
use std::{collections::HashSet, fs::File, io::Read, path::PathBuf, sync::Arc};

const FONT_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Eq, Hash, PartialEq)]
struct Face {
  path: PathBuf,
  index: u32,
}

impl Face {
  fn read(&self) -> Option<FontData> {
    let mut bytes = Vec::new();
    File::open(&self.path)
      .ok()?
      .take(FONT_LIMIT + 1)
      .read_to_end(&mut bytes)
      .ok()?;
    if bytes.len() as u64 > FONT_LIMIT {
      return None;
    }
    // egui panics on an invalid font, so validate OS files with its parser.
    ab_glyph::FontRef::try_from_slice_and_index(&bytes, self.index).ok()?;
    let mut data = FontData::from_owned(bytes);
    data.index = self.index;
    Some(data)
  }
}

pub(super) fn install(ctx: &egui::Context) {
  let mut fonts = FontDefinitions::default();
  let mut loaded = HashSet::new();
  for choices in platform_choices() {
    for face in choices {
      if loaded.contains(&face) {
        break;
      }
      if let Some(data) = face.read() {
        let name = format!("system-fallback-{}", loaded.len());
        fonts.font_data.insert(name.clone(), Arc::new(data));
        for family in fonts.families.values_mut() {
          family.push(name.clone());
        }
        loaded.insert(face);
        break;
      }
    }
  }
  if !loaded.is_empty() {
    ctx.set_fonts(fonts);
  }
}

#[cfg(any(target_os = "macos", windows))]
fn choices_from_directory(dir: &std::path::Path, names: &[&[&str]]) -> Vec<Vec<Face>> {
  names
    .iter()
    .map(|group| {
      group
        .iter()
        .map(|name| Face {
          path: dir.join(name),
          index: 0,
        })
        .collect()
    })
    .collect()
}

#[cfg(target_os = "macos")]
fn platform_choices() -> Vec<Vec<Face>> {
  let dir = std::path::Path::new("/System/Library/Fonts");
  // Arial Unicode covers common Han, Kana, Hangul, and several Indic scripts.
  // On a machine without it, look for individual system fonts instead.
  let wide = "Supplemental/Arial Unicode.ttf";
  choices_from_directory(
    dir,
    &[
      &[
        wide,
        "PingFang.ttc",
        "Hiragino Sans GB.ttc",
        "STHeiti Medium.ttc",
      ],
      &[wide, "ヒラギノ角ゴシック W3.ttc", "AquaKana.ttc"],
      &[wide, "AppleSDGothicNeo.ttc"],
      &[wide, "GeezaPro.ttc"],
      &[wide, "SFHebrew.ttf"],
      &[wide, "Supplemental/Devanagari Sangam MN.ttc"],
      &["Apple Symbols.ttf"],
    ],
  )
}

#[cfg(windows)]
fn platform_choices() -> Vec<Vec<Face>> {
  let Some(dir) = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")) else {
    return Vec::new();
  };
  choices_from_directory(
    &PathBuf::from(dir).join("Fonts"),
    &[
      &["msyh.ttc", "simsun.ttc", "arialuni.ttf"],
      &["msjh.ttc", "mingliu.ttc", "arialuni.ttf"],
      &["meiryo.ttc", "msgothic.ttc", "arialuni.ttf"],
      &["malgun.ttf", "gulim.ttc", "arialuni.ttf"],
      &["segoeui.ttf", "arial.ttf"],
      &["nirmala.ttf", "mangal.ttf"],
      &["seguisym.ttf"],
    ],
  )
}

#[cfg(target_os = "linux")]
fn platform_choices() -> Vec<Vec<Face>> {
  // Ask fontconfig for the user's preferred installed font for each script.
  // A minimal desktop can omit fontconfig or individual language fonts.
  [
    ("zh-cn", '中'),
    ("zh-tw", '臺'),
    ("ja", 'あ'),
    ("ko", '한'),
    ("ar", 'ع'),
    ("he", 'א'),
    ("hi", 'क'),
    ("bn", 'ক'),
    ("th", 'ก'),
    ("vi", 'ệ'),
  ]
  .into_iter()
  .filter_map(|(language, letter)| {
    use std::process::{Command, Stdio};
    let pattern = format!("sans-serif:lang={language}:charset={:x}", u32::from(letter));
    let output = Command::new("fc-match")
      .args(["--format", "%{file}\n%{index}\n", &pattern])
      .stderr(Stdio::null())
      .output()
      .ok()?;
    if !output.status.success() {
      return None;
    }
    let response = String::from_utf8(output.stdout).ok()?;
    let mut fields = response.lines();
    let path = PathBuf::from(fields.next()?);
    let index = fields.next()?.parse::<u32>().ok()?;
    Some(vec![Face { path, index }])
  })
  .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_choices() -> Vec<Vec<Face>> {
  Vec::new()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
  use super::*;

  #[test]
  fn installed_fonts_render_mixed_cjk_titles_in_egui() {
    if !std::path::Path::new("/System/Library/Fonts/Supplemental/Arial Unicode.ttf").is_file() {
      return;
    }
    let ctx = egui::Context::default();
    install(&ctx);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
      ui.ctx().fonts_mut(|fonts| {
        for font in [
          egui::FontId::proportional(16.0),
          egui::FontId::monospace(16.0),
        ] {
          assert!(fonts.has_glyphs(&font, "窗口 · タイトル · 창 제목 · café"));
        }
      });
    });
    output.textures_delta.clear();
  }
}
