//! What this installation remembers that is not a repository.
//!
//! `settings.json` holds the folders this application manages and, later, the
//! display preferences. Everything about a repository itself — its remote, its
//! encryption key — is read from the repository's own files and never cached
//! here.
//!
//! The file is read once, at startup, and kept in memory as a GPUI global;
//! every change is written straight back. A key this version does not know is
//! left alone, so a later version's settings survive an older one's writes.

use std::fs;
use std::path::{Path, PathBuf};

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One folder of the person's, as this installation remembers it.
///
/// Its sync state is not stored: the remote lives in the folder, and the
/// outcome of a sync belongs to the session that ran it. The time of the last
/// good sync is kept so the list can say when it last worked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRecord {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub last_sync_at: Option<u64>,
}

pub struct Settings {
    file: PathBuf,
    values: Map<String, Value>,
}

impl Global for Settings {}

impl Settings {
    /// Reads `file`. A missing file is the ordinary first run; one that does
    /// not parse is reported and treated the same way rather than refused,
    /// since the folders are elsewhere and intact.
    pub fn load(file: PathBuf) -> Self {
        let values = match fs::read_to_string(&file) {
            Ok(raw) => serde_json::from_str::<Map<String, Value>>(&raw).unwrap_or_else(|error| {
                tracing::warn!(%error, "could not parse the settings");
                Map::new()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(error) => {
                tracing::warn!(%error, "could not read the settings");
                Map::new()
            }
        };
        Self { file, values }
    }

    pub fn install(cx: &mut App) {
        cx.set_global(Self::load(crate::paths::settings_file()));
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Changes the settings and writes them back.
    pub fn update<R>(cx: &mut App, change: impl FnOnce(&mut Self) -> R) -> R {
        let settings = cx.global_mut::<Self>();
        let result = change(settings);
        if let Err(error) = settings.save() {
            tracing::warn!(%error, "could not save the settings");
        }
        result
    }

    pub fn theme(&self) -> Option<String> {
        self.get("theme")
    }

    pub fn set_theme(&mut self, theme: &str) {
        self.set("theme", Some(theme));
    }

    pub fn color(&self) -> Option<String> {
        self.get("color")
    }

    pub fn set_color(&mut self, color: &str) {
        self.set("color", Some(color));
    }

    pub fn interface_size(&self) -> Option<String> {
        self.get("interface_size")
    }

    pub fn set_interface_size(&mut self, size: &str) {
        self.set("interface_size", Some(size));
    }

    /// The colour the system last said is its own, so the next launch starts
    /// in it before the system is asked again.
    pub fn system_color(&self) -> Option<String> {
        self.get("system_color")
    }

    pub fn set_system_color(&mut self, color: Option<String>) {
        self.set("system_color", color);
    }

    fn save(&self) -> std::io::Result<()> {
        write_atomically(&self.file, &Value::Object(self.values.clone()).to_string())
    }

    fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        self.values
            .get(key)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    fn set<T: Serialize>(&mut self, key: &str, value: Option<T>) {
        match value.and_then(|value| serde_json::to_value(value).ok()) {
            Some(value) => self.values.insert(key.to_string(), value),
            None => self.values.remove(key),
        };
    }

    /// The folders this installation manages, in the order they were added.
    pub fn repositories(&self) -> Vec<RepoRecord> {
        self.get("repositories").unwrap_or_default()
    }

    /// Adds a folder unless it is already there. Answers whether it did.
    pub fn add_repository(&mut self, record: RepoRecord) -> bool {
        let mut repositories = self.repositories();
        if repositories
            .iter()
            .any(|existing| existing.path == record.path)
        {
            return false;
        }
        repositories.push(record);
        self.set("repositories", Some(repositories));
        true
    }

    pub fn remove_repository(&mut self, path: &str) {
        let repositories: Vec<RepoRecord> = self
            .repositories()
            .into_iter()
            .filter(|record| record.path != path)
            .collect();
        self.set("repositories", Some(repositories));
    }

    /// Notes that a sync of this folder succeeded at `at` (unix milliseconds).
    /// Used once the sync stage lands.
    #[allow(dead_code)]
    pub fn note_synced(&mut self, path: &str, at: u64) {
        let mut repositories = self.repositories();
        if let Some(record) = repositories.iter_mut().find(|record| record.path == path) {
            record.last_sync_at = Some(at);
            self.set("repositories", Some(repositories));
        }
    }
}

fn write_atomically(file: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = file.with_extension("json.tmp");
    fs::write(&temporary, contents)?;
    fs::rename(&temporary, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tresse-gui-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("settings.json")
    }

    #[test]
    fn a_folder_is_added_once_and_removed_by_its_path() {
        let file = scratch("repositories");
        let mut settings = Settings::load(file.clone());
        assert!(settings.repositories().is_empty());

        let record = RepoRecord {
            path: "/home/someone/notes".into(),
            name: "notes".into(),
            last_sync_at: None,
        };
        assert!(settings.add_repository(record.clone()));
        assert!(!settings.add_repository(record.clone()));
        assert_eq!(settings.repositories(), [record.clone()]);
        settings.save().unwrap();

        assert_eq!(
            settings.repositories(),
            Settings::load(file.clone()).repositories()
        );

        settings.note_synced(&record.path, 42);
        assert_eq!(settings.repositories()[0].last_sync_at, Some(42));

        settings.remove_repository(&record.path);
        assert!(settings.repositories().is_empty());
        settings.save().unwrap();
        assert!(Settings::load(file).repositories().is_empty());
    }

    #[test]
    fn a_key_this_version_does_not_know_survives_a_save() {
        let file = scratch("unknown");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"theme":"dark","later":"kept"}"#).unwrap();
        let mut settings = Settings::load(file.clone());
        settings.add_repository(RepoRecord {
            path: "/notes".into(),
            name: "notes".into(),
            last_sync_at: None,
        });
        settings.save().unwrap();

        let reread: Map<String, Value> =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(reread.get("later").and_then(Value::as_str), Some("kept"));
        assert_eq!(reread.get("theme").and_then(Value::as_str), Some("dark"));
    }
}
