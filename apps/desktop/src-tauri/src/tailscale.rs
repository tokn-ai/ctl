pub use ctl_client::tailscale::TailscaleDiscovery;

#[tauri::command]
pub async fn list_tailscale_devices() -> TailscaleDiscovery {
  ctl_client::tailscale::discover_devices().await
}
