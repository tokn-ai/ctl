//! Shared per-user storage for ctl and ctmux, independent of desktop app IDs.

use std::{io, path::PathBuf};

/// Resolves `~/.tokn/ctl` on every platform without creating it.
///
/// # Errors
/// Returns an error when the current user's home directory is unavailable.
pub fn directory() -> io::Result<PathBuf> {
  dirs::home_dir()
    .map(|home| home.join(".tokn/ctl"))
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "Could not locate the home directory.",
      )
    })
}
