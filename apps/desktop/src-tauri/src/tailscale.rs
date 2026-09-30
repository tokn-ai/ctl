pub use ctl_core::tailscale::TailscaleDiscovery;

#[tauri::command]
pub async fn list_tailscale_devices() -> TailscaleDiscovery {
  ctl_core::tailscale::discover_devices().await
}
