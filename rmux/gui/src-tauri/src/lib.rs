mod commands;
mod dto;
mod error;
mod local_transport;
mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Starts the native rmux application runtime.
///
/// # Panics
///
/// Panics when Tauri cannot initialize or run the configured application.
pub fn run() {
  tauri::Builder::default()
    .manage(state::AppState::default())
    .setup(|app| {
      state::register_main_window_cleanup(app);
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      commands::list_sessions,
      commands::create_session,
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
