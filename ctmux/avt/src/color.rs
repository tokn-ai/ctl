use rgb::RGB8;

#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "ColorState", into = "ColorState")]
pub enum Color {
    Indexed(u8),
    RGB(RGB8),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ColorState {
    Indexed(u8),
    Rgb([u8; 3]),
}

impl From<Color> for ColorState {
    fn from(color: Color) -> Self {
        match color {
            Color::Indexed(index) => Self::Indexed(index),
            Color::RGB(rgb) => Self::Rgb([rgb.r, rgb.g, rgb.b]),
        }
    }
}

impl From<ColorState> for Color {
    fn from(color: ColorState) -> Self {
        match color {
            ColorState::Indexed(index) => Self::Indexed(index),
            ColorState::Rgb([r, g, b]) => Self::rgb(r, g, b),
        }
    }
}

impl Color {
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::RGB(RGB8::new(r, g, b))
    }
}
