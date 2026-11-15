//! Opening the folder a record names.
//!
//! A record is an ordinary path on a desktop, and on a phone a tree URI the
//! system's picker handed over with a grant. Everything above this module —
//! the list, the pages, the sync engine, the connection form — asks for the
//! client and never cares which it is.

use crate::settings::RepoRecord;

/// The client over the record's folder.
pub fn open(record: &RepoRecord) -> Result<tresse_client::Repo, String> {
    #[cfg(target_os = "android")]
    if record.saf {
        let vault = crate::saf::SafVault::open(&record.path)?;
        return Ok(tresse_client::Repo::from_vault(
            std::sync::Arc::new(vault),
            record.name.clone(),
        ));
    }
    #[cfg(not(target_os = "android"))]
    if record.saf {
        return Err("This folder was picked on a phone.".to_string());
    }
    tresse_client::Repo::open_fs(&record.path).map_err(|error| error.to_string())
}

/// The name a folder is shown by: its own, or the whole path when it has
/// none. (On a phone the picker names the folder itself.)
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn folder_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}
