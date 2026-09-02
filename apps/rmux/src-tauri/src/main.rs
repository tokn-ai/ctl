// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
  if let Some(code) = rmux_app_lib::ssh_askpass_exit_code() {
    std::process::exit(code);
  }
  rmux_app_lib::run();
}
