use thiserror::Error;

/// A private terminal observer checkpoint cannot be safely restored.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ObserverStateError {
  #[error("unsupported {component} state version {version}")]
  UnsupportedVersion {
    component: &'static str,
    version: u32,
  },
  #[error("invalid terminal observer state: {0}")]
  InvalidState(&'static str),
}

pub(crate) fn validate_version(
  component: &'static str,
  actual: u32,
  expected: u32,
) -> Result<(), ObserverStateError> {
  if actual == expected {
    Ok(())
  } else {
    Err(ObserverStateError::UnsupportedVersion {
      component,
      version: actual,
    })
  }
}
