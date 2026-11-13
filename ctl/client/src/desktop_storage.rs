//! Desktop storage and update-channel preferences shared by supported platforms.

pub use crate::installation::{app_directory, default_channel, set_channel};

#[cfg(test)]
mod tests;
