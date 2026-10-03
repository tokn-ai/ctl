//! Shared foundations for ctl and ctmux components.

#[cfg(all(feature = "bundles", unix))]
pub mod bundles;
pub mod component;
pub mod connection;
#[cfg(feature = "executable")]
pub mod executable;
pub mod paths;
pub mod protocol;
