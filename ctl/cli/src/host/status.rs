use ctl_client::hosts::{self, HostCatalogDocument, WorkspaceHost};
use serde::Serialize;

use super::{Error, host_index, method_index};

#[derive(Debug, Serialize)]
struct MethodStatus {
  method_id: String,
  method_name: String,
  preferred: bool,
  state: &'static str,
  #[serde(skip_serializing_if = "Option::is_none")]
  message: Option<String>,
}

#[derive(Serialize)]
struct HostView<'a> {
  host: &'a WorkspaceHost,
  statuses: Vec<MethodStatus>,
}

pub(super) async fn display(
  catalog: &HostCatalogDocument,
  selector: Option<&str>,
  method: Option<&str>,
  json: bool,
  detailed: bool,
) -> Result<(), Error> {
  let hosts: Vec<_> = if let Some(selector) = selector {
    vec![&catalog.hosts[host_index(catalog, selector)?]]
  } else {
    catalog.hosts.iter().collect()
  };
  let devices = if hosts
    .iter()
    .any(|host| host_requires_tailscale(catalog, host))
  {
    ctl_client::tailscale::discover_devices().await.devices
  } else {
    Vec::new()
  };
  let mut views = Vec::new();
  let mut jobs = tokio::task::JoinSet::new();
  for host in hosts {
    let selected = method
      .map(|method| method_index(host, Some(method)))
      .transpose()?;
    let view_index = views.len();
    views.push(HostView {
      host,
      statuses: Vec::new(),
    });
    for (index, method) in host.connection_methods.iter().enumerate() {
      if selected.is_some_and(|selected| index != selected) {
        continue;
      }
      let status_index = views[view_index].statuses.len();
      let resolved =
        hosts::resolve(catalog, &host.host_id, Some(&method.method_id)).and_then(|mut resolved| {
          resolved.resolve_tailscale(&devices)?;
          Ok(resolved)
        });
      views[view_index].statuses.push(MethodStatus {
        method_id: method.method_id.clone(),
        method_name: method.name.clone(),
        preferred: host.preferred_method_id.as_ref() == Some(&method.method_id),
        state: "unknown",
        message: None,
      });
      jobs.spawn(async move {
        let result = match resolved {
          Ok(resolved) => observe(resolved.target).await,
          Err(error) => Err(error.to_string()),
        };
        (view_index, status_index, result)
      });
      // Bound simultaneous local control checks even for large catalogs.
      if jobs.len() >= 8 {
        let (view, status, result) = jobs
          .join_next()
          .await
          .unwrap()
          .map_err(|error| Error::Usage(error.to_string()))?;
        apply_result(&mut views, view, status, result);
      }
    }
  }
  while let Some(result) = jobs.join_next().await {
    let (view, status, result) = result.map_err(|error| Error::Usage(error.to_string()))?;
    apply_result(&mut views, view, status, result);
  }
  if json {
    if detailed {
      println!("{}", serde_json::to_string_pretty(&views[0])?);
    } else {
      println!("{}", serde_json::to_string_pretty(&views)?);
    }
  } else if detailed {
    let view = &views[0];
    println!("{}", serde_json::to_string_pretty(&view.host)?);
    print_rows(&views);
  } else {
    print_rows(&views);
  }
  Ok(())
}

fn apply_result(
  views: &mut [HostView<'_>],
  view: usize,
  status: usize,
  result: Result<&'static str, String>,
) {
  apply_status(&mut views[view].statuses[status], result);
}

fn apply_status(status: &mut MethodStatus, result: Result<&'static str, String>) {
  match result {
    Ok(state) => status.state = state,
    Err(message) => status.message = Some(message),
  }
}

fn print_rows(views: &[HostView<'_>]) {
  if views.is_empty() {
    println!("No saved hosts. Add one with: ctl host add NAME DESTINATION");
    return;
  }
  let rows = views.iter().flat_map(|view| {
    view.statuses.iter().map(|status| {
      let method = view
        .host
        .connection_methods
        .iter()
        .find(|method| method.method_id == status.method_id)
        .unwrap();
      [
        view.host.name.clone(),
        view.host.host_id.clone(),
        format!(
          "{}{}",
          status.method_name,
          if status.preferred { "*" } else { "" }
        ),
        status.state.to_owned(),
        method.target.label().to_owned(),
      ]
    })
  });
  println!(
    "{}",
    crate::table::format(["HOST", "ID", "METHOD", "STATUS", "DESTINATION"], rows)
  );
  for view in views {
    for status in &view.statuses {
      if let Some(message) = &status.message {
        println!(
          "{} / {}: {}",
          crate::table::text(&view.host.name),
          crate::table::text(&status.method_name),
          crate::table::text(message)
        );
      }
    }
  }
}

#[cfg(unix)]
async fn observe(target: hosts::ConnectionTargetDto) -> Result<&'static str, String> {
  use ctl_ipc::{ClientMessage, ServerMessage};
  let response = crate::ssh_broker::request_existing(ClientMessage::ConnectionStatus {
    target: target.to_ssh_target().map_err(|error| error.to_string())?,
  })
  .await
  .map_err(|error| error.to_string())?;
  match response {
    Some(ServerMessage::ConnectionStatus {
      manually_disconnected: true,
      ..
    }) => Ok("paused"),
    Some(ServerMessage::ConnectionStatus {
      connected: true, ..
    }) => Ok("connected"),
    None
    | Some(ServerMessage::ConnectionStatus {
      connected: false, ..
    }) => Ok("disconnected"),
    Some(ServerMessage::Error { code, .. }) if code == "ssh_host_disconnected" => Ok("paused"),
    Some(ServerMessage::Error { message, .. }) => Err(message),
    _ => Err("ctld returned an unexpected status response.".into()),
  }
}

#[cfg(not(unix))]
async fn observe(_target: hosts::ConnectionTargetDto) -> Result<&'static str, String> {
  Ok("unsupported")
}

#[cfg(unix)]
pub(super) async fn connect(
  catalog: &HostCatalogDocument,
  selector: &str,
  method: Option<&str>,
) -> Result<(), Error> {
  let host = &catalog.hosts[host_index(catalog, selector)?];
  let index = method_index(host, method)?;
  let mut resolved = hosts::resolve(
    catalog,
    &host.host_id,
    Some(&host.connection_methods[index].method_id),
  )?;
  if resolved.requires_tailscale() {
    resolved.resolve_tailscale(&ctl_client::tailscale::discover_devices().await.devices)?;
  }
  crate::target::ensure_vpn(&resolved.target).await?;
  crate::ssh_broker::ensure_master(resolved.target.to_ssh_target()?).await?;
  println!(
    "Connected {} ({})",
    host.name, host.connection_methods[index].name
  );
  Ok(())
}

#[cfg(unix)]
pub(super) async fn disconnect(
  catalog: &HostCatalogDocument,
  selector: &str,
  method: Option<&str>,
) -> Result<(), Error> {
  use ctl_ipc::{ClientMessage, ServerMessage};
  let host = &catalog.hosts[host_index(catalog, selector)?];
  let selected = method
    .map(|method| method_index(host, Some(method)))
    .transpose()?;
  let devices = if host_requires_tailscale(catalog, host) {
    ctl_client::tailscale::discover_devices().await.devices
  } else {
    Vec::new()
  };
  let mut targets = std::collections::HashSet::new();
  let mut failures = Vec::new();
  for (index, method) in host.connection_methods.iter().enumerate() {
    if selected.is_some_and(|selected| selected != index) {
      continue;
    }
    let mut resolved = hosts::resolve(catalog, &host.host_id, Some(&method.method_id))?;
    // Stop the saved address too, including when device discovery is offline.
    targets.insert(resolved.target.to_ssh_target()?);
    if resolved.requires_tailscale() && resolved.resolve_tailscale(&devices).is_ok() {
      targets.insert(resolved.target.to_ssh_target()?);
    }
  }
  for target in targets {
    let destination = target.destination.clone();
    match crate::ssh_broker::request(ClientMessage::DisconnectMaster { target }).await {
      Ok(ServerMessage::MasterDisconnected) => {}
      Ok(_) => failures.push(format!("{destination}: unexpected disconnect response")),
      Err(error) => failures.push(format!("{destination}: {error}")),
    }
  }
  if !failures.is_empty() {
    return Err(Error::Usage(failures.join("\n")));
  }
  println!("Disconnected {}", host.name);
  Ok(())
}

fn host_requires_tailscale(catalog: &HostCatalogDocument, host: &WorkspaceHost) -> bool {
  host.connection_methods.iter().any(|method| {
    hosts::resolve(catalog, &host.host_id, Some(&method.method_id))
      .is_ok_and(|resolved| resolved.requires_tailscale())
  })
}
