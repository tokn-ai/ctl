mod about;
mod browser;
mod command_menu;
mod commands;
mod component_update;
mod credentials;
mod daemon_helper;
mod dto;
mod error;
mod keybindings;
mod local_transport;
#[cfg(target_os = "macos")]
mod native_menu;
mod port_forwarding;
#[cfg(unix)]
mod remote_agent;
#[cfg(unix)]
mod ssh_auth;
#[cfg(not(unix))]
#[path = "ssh_auth/unsupported.rs"]
mod ssh_auth;
mod ssh_config;
mod ssh_identity;
mod ssh_reachability;
mod state;
mod tailscale;
mod task_definitions;
mod tasks;
mod transport;
mod vpn;
mod workspace;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager as _;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Starts the native ctmux application runtime.
///
/// # Panics
///
/// Panics when Tauri cannot initialize or run the configured application.
pub fn run() {
  let builder = tauri::Builder::default()
    .manage(state::AppState::default())
    .manage(tasks::TaskStreams::default());
  #[cfg(target_os = "macos")]
  let builder = builder
    .menu(native_menu::build)
    .on_menu_event(|app_handle, event| native_menu::handle(app_handle, &event));

  builder
    .setup(setup_desktop)
    .on_page_load(track_attachment_document)
    .on_window_event(cleanup_destroyed_window)
    .invoke_handler(tauri::generate_handler![
      about::get_component_versions,
      browser::open_external_url,
      component_update::update_components,
      component_update::cancel_component_update,
      component_update::catalog::check_component_updates,
      about::bundles::get_component_bundles,
      about::bundles::select_component_bundle,
      about::restart::preflight_component_action,
      about::restart::execute_component_action,
      about::remote_actions::reconnect::ack_component_reconnect,
      credentials::list_saved_credentials,
      credentials::forget_saved_credential,
      credentials::import_credential_metadata,
      credentials::identities::list_credential_identity_files,
      credentials::identities::save_identity_passphrase,
      credentials::identities::forget_identity_passphrase,
      tasks::task_request,
      tasks::restart_task_daemon,
      tasks::watch_task_logs,
      tasks::cancel_task_logs,
      tasks::acknowledge_task_log,
      task_definitions::load_task_definitions,
      task_definitions::save_task_definition,
      task_definitions::remove_task_definition,
      workspace::load_workspace,
      workspace::update_workspace,
      workspace::load_hosts,
      workspace::update_hosts,
      keybindings::load_keybindings,
      keybindings::save_keybindings,
      vpn::load_vpn_connections,
      vpn::save_vpn_connection,
      vpn::delete_vpn_connection,
      vpn::connect_vpn,
      vpn::vpn_status,
      vpn::stop_vpn,
      vpn::open_vpn_sign_in,
      vpn::begin_vpn_enrollment,
      vpn::vpn_enrollment_status,
      vpn::save_vpn_enrollment,
      vpn::cancel_vpn_enrollment,
      command_menu::sync_command_menu,
      commands::inspection::inspect_known_sessions,
      commands::list_sessions,
      commands::archives::session_archive,
      commands::cache::session_cache,
      commands::views::session_view,
      commands::list_ssh_config_hosts,
      tailscale::list_tailscale_devices,
      commands::list_ssh_identity_files,
      commands::save_ssh_config_host,
      ssh_auth::commands::probe_ssh_host,
      ssh_auth::commands::install_remote_agent,
      ssh_auth::commands::restart_remote_ctmux,
      ssh_auth::commands::respond_ssh_prompt,
      ssh_auth::commands::cancel_ssh_probe,
      ssh_auth::commands::forget_ssh_credentials,
      ssh_auth::commands::ssh_connection_status,
      ssh_reachability::ssh_reachability,
      ssh_auth::commands::disconnect_ssh_host,
      ssh_auth::commands::configure_port_forward,
      ssh_auth::commands::list_port_forwards,
      ssh_auth::commands::list_remote_listeners,
      port_forwarding::check_local_port,
      commands::create_session,
      commands::kill_session,
      commands::restart_local_daemon,
      commands::open_attachment,
      commands::cancel_attachment_open,
      commands::send_input,
      commands::resize_attachment,
      commands::resize_attachment_pane,
      commands::resize_attachment_divider,
      commands::set_attachment_view_zoom,
      commands::acquire_attachment_lease,
      commands::release_attachment_lease,
      commands::acknowledge_attachment_event,
      commands::request_attachment_checkpoint,
      commands::detach_attachment,
      commands::close_attachment_window,
    ])
    .build(tauri::generate_context!())
    .expect("failed to build ctmux")
    .run(quit_after_detaching());
}

fn setup_desktop(_app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
  daemon_helper::register()?;
  component_update::catalog::start();
  Ok(())
}

fn track_attachment_document(
  webview: &tauri::Webview,
  payload: &tauri::webview::PageLoadPayload<'_>,
) {
  if payload.event() == tauri::webview::PageLoadEvent::Started {
    webview
      .state::<state::AppState>()
      .attachment_page_started(webview.window().label());
  }
}

fn quit_after_detaching() -> impl FnMut(&tauri::AppHandle, tauri::RunEvent) {
  let finished = Arc::new(AtomicBool::new(false));
  let mut draining = false;
  move |app, event| {
    if let tauri::RunEvent::ExitRequested { api, code, .. } = event
      && code != Some(tauri::RESTART_EXIT_CODE)
      && !finished.load(Ordering::Acquire)
    {
      api.prevent_exit();
      if draining {
        return;
      }
      draining = true;
      let cleanup = app
        .state::<state::AppState>()
        .retire_all_attachment_windows();
      let app = app.clone();
      let finished = Arc::clone(&finished);
      tauri::async_runtime::spawn(async move {
        cleanup.await;
        finished.store(true, Ordering::Release);
        app.exit(code.unwrap_or(0));
      });
    }
  }
}

fn cleanup_destroyed_window(window: &tauri::Window, event: &tauri::WindowEvent) {
  if matches!(event, tauri::WindowEvent::Destroyed) {
    // Invalidate immediately, before queued IPC commands can reserve a slot.
    drop(
      window
        .state::<state::AppState>()
        .retire_attachment_window(window.label()),
    );
    ssh_auth::cancel_window(window.label());
    let streams = window.state::<tasks::TaskStreams>().inner().clone();
    let label = window.label().to_owned();
    component_update::close_window(&label);
    tauri::async_runtime::spawn(async move {
      tokio::join!(
        streams.close_window(&label),
        vpn::close_window(&label),
        about::close_window(&label)
      );
    });
  }
}
