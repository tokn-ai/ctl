//! Independent native Windows capture providers.
//!
//! DXGI captures displays; WGC captures compositor window surfaces. Both emit
//! physical-resolution PPM frames and share private D3D11 readback utilities.

pub mod dxgi;
pub mod wgc;

mod pixels;
mod readback;
