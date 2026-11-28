//! Private resumable lanes. The surrounding SDK/SSH stream keeps its original
//! protocol. An ordinary transport failure always ends the lane. Only a SWITCH
//! received from its authenticated predecessor authorizes a resume.
mod client;
mod records;

#[cfg(any(target_os = "linux", windows))]
pub(super) use client::runtime as background;
pub(super) use client::{Stream, connect};
pub(super) use records::server;

// A private controller envelope can contain a complete 64 KiB SDK request.
const MAX_REQUEST: usize = 2 * 65536;
const MAX_DATA: usize = 64 * 1024;
const HEADER: usize = 12;
