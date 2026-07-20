//! Tauri IPC commands.

use tauri::State;
use crate::ServerState;

#[tauri::command]
fn get_server_url(state: State<ServerState>) -> String {
    format!("http://127.0.0.1:{}", state.server_port)
}
