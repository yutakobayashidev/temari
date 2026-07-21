use std::path::Path;

use directories::ProjectDirs;
use serde::Serialize;

mod managed_api;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigLocation {
    path: Option<String>,
    default_path: String,
}

#[tauri::command]
fn default_config_location() -> Result<ConfigLocation, String> {
    let directories = ProjectDirs::from("dev", "yutakobayashidev", "temari")
        .ok_or_else(|| "could not determine the user configuration directory".to_owned())?;
    let default_path = directories.config_dir().join("config.toml");
    let path = if default_path.is_file() {
        Some(path_to_string(&default_path.canonicalize().map_err(
            |error| {
                format!(
                    "could not read model configuration {:?}: {error}",
                    default_path.display().to_string()
                )
            },
        )?)?)
    } else {
        None
    };
    Ok(ConfigLocation {
        path,
        default_path: path_to_string(&default_path)?,
    })
}

fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(managed_api::ManagedAppState::default())
        .invoke_handler(tauri::generate_handler![
            default_config_location,
            managed_api::managed_list_workspaces,
            managed_api::managed_propose_workspace,
            managed_api::managed_preview_workspace,
            managed_api::managed_apply_workspace,
            managed_api::managed_preview_library_edit,
            managed_api::managed_apply_library_edit,
            managed_api::managed_undo_library_edit,
            managed_api::managed_resume_library_edit,
            managed_api::managed_get_workspace,
            managed_api::managed_set_workspace_enabled,
            managed_api::managed_run,
            managed_api::managed_reprocess,
            managed_api::managed_schedule_status,
            managed_api::managed_schedule_enable,
            managed_api::managed_schedule_disable,
            managed_api::managed_history,
            managed_api::managed_undo_session,
            managed_api::managed_undo_move
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Temari desktop");
}
