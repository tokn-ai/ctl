use crate::error::{CommandErrorDto, CommandResult};
use ctmux_client::archive::{ArchiveStore, SessionArchive};
use ctmux_client::cache::CacheStore;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ArchiveRequest {
  action: ArchiveAction,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveAction {
  List,
  Read {
    host_key: String,
    session_id: String,
    terminal_id: String,
    offset: String,
  },
  Save {
    archive: SessionArchive,
  },
  Delete {
    host_key: String,
    session_id: String,
  },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveResponse {
  List {
    archives: Vec<SessionArchive>,
  },
  Output {
    lines: Vec<String>,
    next_offset: Option<String>,
  },
  Saved,
  Deleted,
}

#[tauri::command]
pub async fn session_archive(request: ArchiveRequest) -> CommandResult<ArchiveResponse> {
  tokio::task::spawn_blocking(move || {
    let store = ArchiveStore::for_client("desktop")?;
    match request.action {
      ArchiveAction::List => {
        let mut archives = store.list()?;
        for archive in CacheStore::for_client("desktop")?.archives()? {
          archives.retain(|previous| {
            previous.host_key != archive.host_key || previous.session_id != archive.session_id
          });
          archives.push(archive);
        }
        archives.sort_by_key(|archive| std::cmp::Reverse(archive.archived_at_ms));
        Ok(ArchiveResponse::List { archives })
      }
      ArchiveAction::Read {
        host_key,
        session_id,
        terminal_id,
        offset,
      } => {
        let offset = offset
          .parse::<u64>()
          .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        if let Some(page) = CacheStore::for_client("desktop")?.read_archive(
          &host_key,
          &session_id,
          &terminal_id,
          offset,
        )? {
          Ok(ArchiveResponse::Output {
            lines: page.lines,
            next_offset: page.next_offset.map(|offset| offset.to_string()),
          })
        } else {
          let lines = store
            .list()?
            .into_iter()
            .find(|archive| archive.host_key == host_key && archive.session_id == session_id)
            .and_then(|archive| {
              archive
                .terminals
                .into_iter()
                .find(|pane| pane.terminal_id == terminal_id)
            })
            .map_or_else(Vec::new, |pane| pane.lines);
          Ok(ArchiveResponse::Output {
            lines,
            next_offset: None,
          })
        }
      }
      ArchiveAction::Delete {
        host_key,
        session_id,
      } => {
        CacheStore::for_client("desktop")?.delete_archive(&host_key, &session_id)?;
        store.delete(&host_key, &session_id)?;
        Ok(ArchiveResponse::Deleted)
      }
      ArchiveAction::Save { archive } => {
        store.save(archive)?;
        Ok(ArchiveResponse::Saved)
      }
    }
  })
  .await
  .map_err(CommandErrorDto::backend)?
  .map_err(|error: std::io::Error| CommandErrorDto::backend(error))
}
