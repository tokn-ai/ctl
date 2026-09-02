mod commands;
mod dto;
mod error;
mod local_transport;
#[cfg(target_os = "macos")]
mod native_menu;
#[cfg(unix)]
mod ssh_auth;
#[cfg(not(unix))]
#[path = "ssh_auth/unsupported.rs"]
mod ssh_auth;
mod ssh_config;
mod ssh_identity;
mod state;
mod transport;
mod workspace;

pub use ssh_auth::helper_exit_code as ssh_askpass_exit_code;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Starts the native rmux application runtime.
///
/// # Panics
///
/// Panics when Tauri cannot initialize or run the configured application.
pub fn run() {
  let builder = tauri::Builder::default().manage(state::AppState::default());
  #[cfg(target_os = "macos")]
  let builder = builder
    .menu(native_menu::build)
    .on_menu_event(|app_handle, event| native_menu::handle(app_handle, &event));

  builder
    .setup(|app| {
      state::register_main_window_cleanup(app);
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      workspace::load_workspace,
      workspace::update_workspace,
      commands::inspection::inspect_known_sessions,
      commands::list_sessions,
      commands::list_ssh_config_hosts,
      commands::list_ssh_identity_files,
      commands::save_ssh_config_host,
      ssh_auth::commands::probe_ssh_host,
      ssh_auth::commands::respond_ssh_prompt,
      ssh_auth::commands::cancel_ssh_probe,
      ssh_auth::commands::forget_ssh_credentials,
      commands::create_session,
      commands::kill_session,
      commands::restart_local_daemon,
      commands::open_attachment,
      commands::send_input,
      commands::resize_attachment,
      commands::acquire_attachment_lease,
      commands::release_attachment_lease,
      commands::acknowledge_attachment_event,
      commands::detach_attachment,
    ])
    .run(tauri::generate_context!())
    .expect("failed to run rmux");
}
