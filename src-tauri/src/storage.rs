//! Settings persistence in the app data directory.
//!
//! App-wide settings (appearance, AI model, ...) live in `{app_data}/settings.json`
//! and also apply to single-file windows. Library-specific settings live in
//! `{app_data}/folders/{key}.json`, where `key` is a stable hash of the canonical
//! folder path. Older Scratch versions stored everything in `{folder}/.scratch/settings.json`;
//! that file is imported once and is never written again.

use crate::Settings;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Settings that belong to one notes folder (library).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSettings {
    pub git_enabled: Option<bool>,
    pub pinned_note_ids: Option<Vec<String>>,
    pub default_note_name: Option<String>,
    pub folders_enabled: Option<bool>,
    pub ignored_patterns: Option<Vec<String>>,
}

impl FolderSettings {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            git_enabled: settings.git_enabled,
            pinned_note_ids: settings.pinned_note_ids.clone(),
            default_note_name: settings.default_note_name.clone(),
            folders_enabled: settings.folders_enabled,
            ignored_patterns: settings.ignored_patterns.clone(),
        }
    }

    pub fn apply_to(self, settings: &mut Settings) {
        settings.git_enabled = self.git_enabled;
        settings.pinned_note_ids = self.pinned_note_ids;
        settings.default_note_name = self.default_note_name;
        settings.folders_enabled = self.folders_enabled;
        settings.ignored_patterns = self.ignored_patterns;
    }
}

/// On-disk format of a per-folder settings file. `path` is for humans and debugging.
#[derive(Serialize, Deserialize)]
struct FolderSettingsFile {
    path: String,
    #[serde(flatten)]
    settings: FolderSettings,
}

/// Returns a copy of `settings` without the folder-specific fields.
pub fn global_part(settings: &Settings) -> Settings {
    let mut global = settings.clone();
    FolderSettings::default().apply_to(&mut global);
    global
}

/// Stable file key for a canonical folder path (FNV-1a 64-bit, hex).
/// `DefaultHasher` is not used because its output may change between Rust versions.
pub fn folder_key(canonical_folder: &Path) -> String {
    let hash = canonical_folder
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{:016x}", hash)
}

/// Settings merged from the stored files, plus which files a legacy import must write.
pub struct Resolved {
    pub settings: Settings,
    pub write_global: bool,
    pub write_folder: bool,
}

/// Merge stored global and folder settings. A missing part is imported from the
/// legacy `.scratch/settings.json` content, if there is one.
pub fn resolve(
    global: Option<Settings>,
    folder: Option<FolderSettings>,
    legacy: Option<Settings>,
) -> Resolved {
    let write_global = global.is_none() && legacy.is_some();
    let write_folder = folder.is_none() && legacy.is_some();
    let folder = folder
        .or_else(|| legacy.as_ref().map(FolderSettings::from_settings))
        .unwrap_or_default();
    let mut settings = global.or(legacy).unwrap_or_default();
    folder.apply_to(&mut settings);
    Resolved {
        settings,
        write_global,
        write_folder,
    }
}

fn global_settings_path(app_data: &Path) -> PathBuf {
    app_data.join("settings.json")
}

fn canonical_folder(folder: &str) -> PathBuf {
    // Fall back to the configured path when the folder is temporarily unavailable.
    Path::new(folder)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(folder))
}

fn folder_settings_path(app_data: &Path, canonical_folder: &Path) -> PathBuf {
    app_data
        .join("folders")
        .join(format!("{}.json", folder_key(canonical_folder)))
}

fn legacy_settings_path(folder: &str) -> PathBuf {
    Path::new(folder).join(".scratch").join("settings.json")
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn write_folder_settings(
    app_data: &Path,
    canonical_folder: &Path,
    settings: FolderSettings,
) -> anyhow::Result<()> {
    let file = FolderSettingsFile {
        path: canonical_folder.to_string_lossy().into_owned(),
        settings,
    };
    write_json(&folder_settings_path(app_data, canonical_folder), &file)
}

/// Load the global settings merged with the settings of `folder` (if any).
/// Imports a legacy `.scratch/settings.json` once, when a stored part is missing.
pub fn load(app_data: &Path, folder: Option<&str>) -> Settings {
    let global = read_json::<Settings>(&global_settings_path(app_data));
    let Some(folder) = folder else {
        return resolve(global, None, None).settings;
    };

    let canonical = canonical_folder(folder);
    let stored_folder = read_json::<FolderSettingsFile>(&folder_settings_path(app_data, &canonical))
        .map(|file| file.settings);
    let legacy = if global.is_none() || stored_folder.is_none() {
        read_json::<Settings>(&legacy_settings_path(folder))
    } else {
        None
    };

    let resolved = resolve(global, stored_folder, legacy);
    if resolved.write_global {
        if let Err(e) = write_json(&global_settings_path(app_data), &global_part(&resolved.settings)) {
            eprintln!("Failed to import legacy settings: {}", e);
        }
    }
    if resolved.write_folder {
        let folder_settings = FolderSettings::from_settings(&resolved.settings);
        if let Err(e) = write_folder_settings(app_data, &canonical, folder_settings) {
            eprintln!("Failed to import legacy folder settings: {}", e);
        }
    }
    resolved.settings
}

/// Save the global part of `settings`, and the folder part when a folder is open.
pub fn save(app_data: &Path, folder: Option<&str>, settings: &Settings) -> anyhow::Result<()> {
    write_json(&global_settings_path(app_data), &global_part(settings))?;
    if let Some(folder) = folder {
        write_folder_settings(
            app_data,
            &canonical_folder(folder),
            FolderSettings::from_settings(settings),
        )?;
    }
    Ok(())
}

/// Maximum number of entries in the recent files list.
pub const MAX_RECENT_FILES: usize = 10;

/// Move `path` to the front of the recent files list, without duplicates.
pub fn push_recent_file(recent: &mut Vec<String>, path: String) {
    recent.retain(|existing| *existing != path);
    recent.insert(0, path);
    recent.truncate(MAX_RECENT_FILES);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditorFontSettings, TextDirection};

    fn sample_settings() -> Settings {
        Settings {
            theme: crate::ThemeSettings {
                mode: "dark".to_string(),
                ..Default::default()
            },
            editor_font: Some(EditorFontSettings {
                base_font_size: Some(18.0),
                ..Default::default()
            }),
            git_enabled: Some(true),
            pinned_note_ids: Some(vec!["a".to_string()]),
            text_direction: Some(TextDirection::Rtl),
            editor_width: Some("wide".to_string()),
            default_note_name: Some("Note {counter}".to_string()),
            interface_zoom: Some(1.1),
            custom_editor_width_px: Some(900),
            sidebar_width_px: Some(300),
            ollama_model: Some("qwen3:8b".to_string()),
            pi_model: Some("anthropic/claude-sonnet-4-5".to_string()),
            pi_thinking: Some("low".to_string()),
            pi_path: None,
            folders_enabled: Some(true),
            ignored_patterns: Some(vec!["node_modules".to_string()]),
            custom_colors_light: None,
            custom_colors_dark: None,
        }
    }

    fn json(settings: &Settings) -> serde_json::Value {
        serde_json::to_value(settings).unwrap()
    }

    #[test]
    fn folder_key_is_stable_and_distinct() {
        // Published FNV-1a 64 test vectors. The key must never change, or users
        // lose their folder settings.
        assert_eq!(folder_key(Path::new("")), "cbf29ce484222325");
        assert_eq!(folder_key(Path::new("a")), "af63dc4c8601ec8c");
        assert_ne!(folder_key(Path::new("/a")), folder_key(Path::new("/b")));
    }

    #[test]
    fn global_part_drops_folder_fields_only() {
        let global = global_part(&sample_settings());
        assert_eq!(FolderSettings::from_settings(&global), FolderSettings::default());
        assert_eq!(global.theme.mode, "dark");
        assert_eq!(global.sidebar_width_px, Some(300));
        assert_eq!(global.ollama_model.as_deref(), Some("qwen3:8b"));
    }

    #[test]
    fn split_and_merge_round_trip() {
        let original = sample_settings();
        let mut merged = global_part(&original);
        FolderSettings::from_settings(&original).apply_to(&mut merged);
        assert_eq!(json(&merged), json(&original));
    }

    #[test]
    fn resolve_uses_stored_files_and_ignores_legacy() {
        let mut legacy = sample_settings();
        legacy.theme.mode = "light".to_string();
        let folder = FolderSettings {
            folders_enabled: Some(false),
            ..Default::default()
        };

        let resolved = resolve(Some(global_part(&sample_settings())), Some(folder), Some(legacy));

        assert!(!resolved.write_global);
        assert!(!resolved.write_folder);
        assert_eq!(resolved.settings.theme.mode, "dark");
        assert_eq!(resolved.settings.folders_enabled, Some(false));
        assert_eq!(resolved.settings.pinned_note_ids, None);
    }

    #[test]
    fn resolve_imports_legacy_when_nothing_is_stored() {
        let resolved = resolve(None, None, Some(sample_settings()));

        assert!(resolved.write_global);
        assert!(resolved.write_folder);
        assert_eq!(json(&resolved.settings), json(&sample_settings()));
    }

    #[test]
    fn resolve_imports_only_folder_part_when_global_exists() {
        let mut global = Settings::default();
        global.theme.mode = "light".to_string();

        let resolved = resolve(Some(global), None, Some(sample_settings()));

        assert!(!resolved.write_global);
        assert!(resolved.write_folder);
        assert_eq!(resolved.settings.theme.mode, "light");
        assert_eq!(resolved.settings.interface_zoom, None);
        assert_eq!(resolved.settings.pinned_note_ids, Some(vec!["a".to_string()]));
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("itch-storage-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn load_imports_legacy_once_and_never_writes_to_scratch_dir() {
        let root = temp_root("migrate");
        let app_data = root.join("app");
        let notes = root.join("notes");
        let legacy_path = notes.join(".scratch").join("settings.json");
        write_json(&legacy_path, &sample_settings()).unwrap();
        let notes_str = notes.to_str().unwrap();

        let loaded = load(&app_data, Some(notes_str));
        assert_eq!(json(&loaded), json(&sample_settings()));

        // A changed legacy file is ignored after the import.
        let mut changed = sample_settings();
        changed.theme.mode = "light".to_string();
        write_json(&legacy_path, &changed).unwrap();
        let legacy_before = std::fs::read_to_string(&legacy_path).unwrap();
        let mut reloaded = load(&app_data, Some(notes_str));
        assert_eq!(reloaded.theme.mode, "dark");

        reloaded.pinned_note_ids = Some(vec!["b".to_string()]);
        save(&app_data, Some(notes_str), &reloaded).unwrap();
        assert_eq!(std::fs::read_to_string(&legacy_path).unwrap(), legacy_before);
        assert_eq!(
            load(&app_data, Some(notes_str)).pinned_note_ids,
            Some(vec!["b".to_string()])
        );

        let folder_file = folder_settings_path(&app_data, &notes.canonicalize().unwrap());
        let stored: serde_json::Value = read_json(&folder_file).unwrap();
        assert_eq!(stored["path"], notes.canonicalize().unwrap().to_str().unwrap());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn load_does_not_create_scratch_dir() {
        let root = temp_root("no-scratch");
        let app_data = root.join("app");
        let notes = root.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let notes_str = notes.to_str().unwrap();

        let settings = load(&app_data, Some(notes_str));
        save(&app_data, Some(notes_str), &settings).unwrap();

        assert!(!notes.join(".scratch").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn global_settings_apply_without_folder_and_folder_settings_stay_per_folder() {
        let root = temp_root("split");
        let app_data = root.join("app");
        let notes_a = root.join("a");
        let notes_b = root.join("b");
        std::fs::create_dir_all(&notes_a).unwrap();
        std::fs::create_dir_all(&notes_b).unwrap();

        save(&app_data, Some(notes_a.to_str().unwrap()), &sample_settings()).unwrap();

        let no_folder = load(&app_data, None);
        assert_eq!(no_folder.theme.mode, "dark");
        assert_eq!(no_folder.pinned_note_ids, None);

        let other_folder = load(&app_data, Some(notes_b.to_str().unwrap()));
        assert_eq!(other_folder.theme.mode, "dark");
        assert_eq!(other_folder.git_enabled, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn push_recent_file_puts_newest_first_without_duplicates() {
        let mut recent = vec!["/a.md".to_string(), "/b.md".to_string()];

        push_recent_file(&mut recent, "/b.md".to_string());
        assert_eq!(recent, vec!["/b.md", "/a.md"]);

        push_recent_file(&mut recent, "/c.md".to_string());
        assert_eq!(recent, vec!["/c.md", "/b.md", "/a.md"]);
    }

    #[test]
    fn push_recent_file_keeps_at_most_max_entries() {
        let mut recent: Vec<String> = (0..MAX_RECENT_FILES).map(|i| format!("/{}.md", i)).collect();

        push_recent_file(&mut recent, "/new.md".to_string());

        assert_eq!(recent.len(), MAX_RECENT_FILES);
        assert_eq!(recent[0], "/new.md");
        assert!(!recent.contains(&format!("/{}.md", MAX_RECENT_FILES - 1)));
    }

    #[test]
    fn resolve_without_any_source_gives_defaults_and_writes_nothing() {
        let resolved = resolve(None, None, None);

        assert!(!resolved.write_global);
        assert!(!resolved.write_folder);
        assert_eq!(json(&resolved.settings), json(&Settings::default()));
    }
}
