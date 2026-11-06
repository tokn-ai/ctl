use super::{AttachmentRecord, LayoutExt, Session, SessionRegistry, lock};
use ctl_core::presence::{ClientIdentity, bounded_label};
use ctmux_proto::SessionViewer;
use std::collections::BTreeMap;

/// Old clients supplied application metadata but no process identity.
#[derive(Clone)]
pub(crate) enum AttachmentPeer {
  Process(ClientIdentity),
  Legacy { name: String, version: String },
}

impl AttachmentPeer {
  pub(crate) fn legacy(name: &str, version: &str) -> Self {
    Self::Legacy {
      name: bounded_label(name, 64).unwrap_or_else(|| "unknown".into()),
      version: bounded_label(version, 64).unwrap_or_else(|| "unknown".into()),
    }
  }

  fn viewer(&self, id: &str) -> SessionViewer {
    let (identity, legacy) = match self {
      Self::Process(identity) => (identity.clone(), false),
      Self::Legacy { name, version } => (
        ClientIdentity {
          instance_id: id.to_owned(),
          client_name: name.clone(),
          client_version: version.clone(),
          device_name: None,
        },
        true,
      ),
    };
    SessionViewer {
      identity,
      legacy,
      connected_attachments: 0,
      reconnecting_attachments: 0,
    }
  }
}

/// The layout determines membership even for hidden or freshly promoted panes.
pub(super) fn session_viewers(registry: &SessionRegistry, session: &Session) -> Vec<SessionViewer> {
  let mut viewers = BTreeMap::new();
  for id in session.view.layout.terminal_ids() {
    if let Some(terminal) = registry.terminals.get(&id) {
      for record in lock(&terminal.attachments).values() {
        observe(&mut viewers, record);
      }
    }
  }
  viewers.into_values().collect()
}

fn observe(viewers: &mut BTreeMap<(bool, String), SessionViewer>, record: &AttachmentRecord) {
  let viewer = record.peer.viewer(&record.attachment_id);
  let key = (viewer.legacy, viewer.identity.instance_id.clone());
  let viewer = viewers.entry(key).or_insert(viewer);
  if record.connected {
    viewer.connected_attachments += 1;
  } else {
    viewer.reconnecting_attachments += 1;
  }
}
