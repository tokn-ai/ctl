mod about;
mod browser;
mod command_menu;
mod command_registry;
mod commands;
mod component_update;
mod credentials;
mod daemon_helper;
#[cfg(unix)]
mod development_installation;
mod dto;
mod error;
mod files;
mod keybindings;
mod local_transport;
#[cfg(target_os = "macos")]
mod native_menu;
mod port_forwarding;
mod preview_windows;
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
  #[cfg(unix)]
  let _development_installation = development_installation::retain();
  let builder = tauri::Builder::default()
    .manage(state::AppState::default())
    .manage(files::FileRequests::default())
    .manage(preview_windows::PreviewWindows::default())
    .manage(tasks::TaskStreams::default());
  #[cfg(target_os = "macos")]
  let builder = builder
    .menu(native_menu::build)
    .on_menu_event(|app_handle, event| native_menu::handle(app_handle, &event));

  command_registry::register(builder)
    .setup(setup_desktop)
    .on_page_load(track_attachment_document)
    .on_window_event(cleanup_destroyed_window)
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
      .state::<files::FileRequests>()
      .page_started(webview.window().label());
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
    window
      .state::<files::FileRequests>()
      .close_window(window.label());
    window
      .state::<preview_windows::PreviewWindows>()
      .close_window(window.label());
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
