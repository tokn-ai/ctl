//! Shared foundations for ctl and ctmux components.

#[path = "bundles/layout.rs"]
pub mod bundle_layout;
#[cfg(all(feature = "bundles", any(unix, windows)))]
pub mod bundles;
pub mod component;
pub mod component_update;
pub mod connection;
#[cfg(feature = "development")]
pub mod development;
#[cfg(feature = "executable")]
pub mod executable;
#[cfg(all(feature = "bundles", any(unix, windows)))]
pub mod installations;
pub mod paths;
pub mod presence;
#[cfg(all(feature = "bundles", any(unix, windows)))]
pub mod private_fs;
pub mod protocol;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;

#[cfg(feature = "observability")]
pub mod observability;
