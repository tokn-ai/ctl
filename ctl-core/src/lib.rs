//! Shared foundations for ctl and ctmux components.

#[cfg(all(feature = "bundles", unix))]
pub mod bundles;
pub mod component;
pub mod component_update;
pub mod connection;
#[cfg(feature = "executable")]
pub mod executable;
pub mod paths;
pub mod protocol;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;

#[cfg(feature = "observability")]
pub mod observability;
