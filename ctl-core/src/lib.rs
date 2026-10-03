//! Shared foundations for ctl and ctmux components.

#[cfg(all(feature = "bundles", unix))]
pub mod bundles;
pub mod component;
#[cfg(feature = "executable")]
pub mod executable;
pub mod paths;
pub mod protocol;
