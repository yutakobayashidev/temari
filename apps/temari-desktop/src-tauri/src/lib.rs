use std::{collections::HashSet, path::Path};

use directories::{ProjectDirs, UserDirs};
use serde::Serialize;

mod managed_api;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigLocation {
    path: Option<String>,
    default_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DefaultSourceLocation {
    id: &'static str,
    label: &'static str,
    path: String,
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

#[tauri::command]
fn default_source_locations() -> Result<Vec<DefaultSourceLocation>, String> {
    let directories =
        UserDirs::new().ok_or_else(|| "could not determine the user directories".to_owned())?;
    source_location_views([
        ("desktop", "Desktop", directories.desktop_dir()),
        ("downloads", "Downloads", directories.download_dir()),
        ("documents", "Documents", directories.document_dir()),
    ])
}

fn source_location_views<const N: usize>(
    candidates: [(&'static str, &'static str, Option<&Path>); N],
) -> Result<Vec<DefaultSourceLocation>, String> {
    let mut seen = HashSet::new();
    let mut locations = Vec::new();
    for (id, label, path) in candidates {
        let Some(path) = path.filter(|path| path.is_dir()) else {
            continue;
        };
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "could not resolve suggested folder {}: {error}",
                path.display()
            )
        })?;
        if managed_api::reject_managed_area_source(&canonical).is_err() {
            continue;
        }
        if seen.insert(canonical.clone()) {
            locations.push(DefaultSourceLocation {
                id,
                label,
                path: path_to_string(&canonical)?,
            });
        }
    }
    Ok(locations)
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
            default_source_locations,
            managed_api::managed_list_workspaces,
            managed_api::managed_propose_workspace,
            managed_api::managed_preview_workspace,
            managed_api::managed_apply_workspace,
            managed_api::managed_preview_library_edit,
            managed_api::managed_apply_library_edit,
            managed_api::managed_undo_library_edit,
            managed_api::managed_redo_library_edit,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn source_suggestions_include_only_existing_unique_directories() {
        let root = tempdir().unwrap();
        let desktop = root.path().join("Desktop");
        let downloads = root.path().join("Downloads");
        let missing = root.path().join("Documents");
        std::fs::create_dir(&desktop).unwrap();
        std::fs::create_dir(&downloads).unwrap();

        let locations = source_location_views([
            ("desktop", "Desktop", Some(desktop.as_path())),
            ("downloads", "Downloads", Some(downloads.as_path())),
            ("documents", "Documents", Some(missing.as_path())),
            ("desktop-copy", "Desktop", Some(desktop.as_path())),
        ])
        .unwrap();

        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].id, "desktop");
        assert_eq!(locations[1].id, "downloads");
    }

    #[test]
    fn source_suggestions_hide_managed_areas() {
        let root = tempdir().unwrap();
        for area in ["Manual Library", "Recents", "AI Library"] {
            std::fs::create_dir(root.path().join(area)).unwrap();
        }
        let recents = root.path().join("Recents");

        let locations =
            source_location_views([("downloads", "Downloads", Some(recents.as_path()))]).unwrap();

        assert!(locations.is_empty());
    }
}
