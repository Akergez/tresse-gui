//! Tresse on the filesystem, for the interface to drive.
//!
//! The engine is `tresse-lib` and the transports are `rdx-sync`'s: this crate
//! is the filesystem client around them, adapted from `tresse-cli` the way
//! Money Manager's `money-tresse` is. Its configuration, repository layout,
//! locking, file index and operation journal stay compatible with the CLI, so
//! one folder can be synced from this application and read by the terminal.
//!
//! The filesystem is reached through a [`Vault`] rather than `std::fs`
//! directly: ordinary paths on a desktop ([`FsVault`]), Android's storage
//! access framework on a phone. That is what lets the person pick any folder
//! on either.
//!
//! The interface drives the operations separately —
//! [`commit`], [`exchange`], [`settle`] — so it can say which of them is
//! running; [`sync`] is all three, for callers that only need the result.

mod cas;
pub mod config;
mod config_share;
mod file_index;
mod fs_writer;
mod operation;
mod os_lock;
mod restore_sink;
mod storage;
pub mod vault;
mod worktree;

mod commands {
    pub mod commit;
    pub mod init;
    pub mod restore;
    pub mod sync;
}

pub use config::{RemoteConfig, StorageType, TresseConfig};
pub use config_share::{KeyChoice, choose_key, marks_new_repository};
pub use rdx_sync_s3::connect_url;
pub use vault::{Entry, FsVault, Meta, Vault, VaultLock, Written};

use std::io;
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;

/// A folder whose history and sync this client drives: the vault it is
/// reached through, and the name it is shown by.
#[derive(Clone)]
pub struct Repo {
    vault: Arc<dyn Vault>,
    label: String,
}

impl Repo {
    /// A repository on the desktop: ordinary paths, created if missing.
    pub fn open_fs(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        let vault = FsVault::open(root)?;
        Ok(Self::from_vault(
            Arc::new(vault),
            root.display().to_string(),
        ))
    }

    /// A repository over any vault: what the phone build and the tests use.
    pub fn from_vault(vault: Arc<dyn Vault>, label: impl Into<String>) -> Self {
        Self {
            vault,
            label: label.into(),
        }
    }

    pub fn vault(&self) -> &Arc<dyn Vault> {
        &self.vault
    }

    /// What the person sees as this repository's place.
    pub fn label(&self) -> &str {
        &self.label
    }
}

pub fn init(repo: &Repo) -> Result<(), String> {
    commands::init::run(repo).map_err(|e| e.to_string())
}

/// Settle interrupted work while holding the same lock used by the CLI.
pub fn recover(repo: &Repo) -> Result<(), String> {
    commands::restore::run(repo, None).map_err(|e| e.to_string())
}

/// The interface's name for [`recover`]: bring the working folder up to what
/// the last sync downloaded.
pub fn settle(repo: &Repo) -> Result<(), String> {
    recover(repo)
}

/// Edit the worktree under the repository lock, settling interrupted operations
/// first. This does not commit or create CAS objects; sync or an explicit commit
/// versions the accumulated edits. The callback must read the file afresh and
/// persist its changes atomically.
pub fn edit(repo: &Repo, write: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    operation::edit_worktree(repo, || write().map_err(Into::into)).map_err(|e| e.to_string())
}

pub fn commit(repo: &Repo) -> Result<(), String> {
    commands::commit::run(repo).map_err(|e| e.to_string())
}

/// Exchange Tresse objects with the configured remote. The working folder is
/// not touched; [`settle`] applies what arrived.
pub fn exchange(repo: &Repo) -> Result<(usize, usize), String> {
    commands::sync::run(repo).map_err(|e| e.to_string())
}

/// Commit local edits, exchange Tresse objects, then apply incoming versions.
/// All remote parameters are re-read from `.tresse/remotes.toml`.
pub fn sync(repo: &Repo) -> Result<(usize, usize), String> {
    commit(repo)?;
    let report = exchange(repo)?;
    settle(repo)?;
    Ok(report)
}

/// Transport injection for offline tests; the key still comes from the file.
pub fn sync_with(
    repo: &Repo,
    remote: &mut impl rdx_sync::RemoteKv,
) -> Result<(usize, usize), String> {
    let config = TresseConfig::load(repo.vault().as_ref()).map_err(|e| e.to_string())?;
    let origin = config.origin().ok_or("remote is not configured")?;
    let key =
        tresse_lib::crypto::decode_key_b64(&origin.encryption_key).map_err(|e| e.to_string())?;
    hydrate(repo, remote, &key)?;
    commit(repo)?;
    let report = commands::sync::exchange(repo, remote, &key).map_err(|e| e.to_string())?;
    hydrate(repo, remote, &key)?;
    settle(repo)?;
    Ok(report)
}

fn hydrate(repo: &Repo, remote: &impl rdx_sync::RemoteKv, key: &[u8; 32]) -> Result<(), String> {
    let mut local = cas::open_local_store(repo).map_err(|e| e.to_string())?;
    let meta = cas::load_merged_meta(&local).map_err(|e| e.to_string())?;
    let missing =
        tresse_lib::partial_sync::content_fetch_plan(&meta, &local).map_err(|e| e.to_string())?;
    tresse_lib::partial_sync::pull_remote_content(&mut local, remote, key, &missing)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// A fresh base64 encryption key, as `encryption_key` wants it.
pub fn generate_encryption_key() -> Result<String, String> {
    let key = rdx_sync::crypto::generate_key().map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(key))
}

/// A fresh native RDX writer source. It is embedded in document stamps, not
/// kept as a Tresse setting.
///
/// Masked into the low 60 bits: the RDX stamp model reserves the upper 4 bits
/// of `source`. `| 1` keeps it nonzero. The terminal version generated its
/// source the same way, so the two are interchangeable.
pub fn generate_source() -> u64 {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut low = [0u8; 8];
    low.copy_from_slice(&bytes[..8]);
    (u64::from_le_bytes(low) & 0x0FFF_FFFF_FFFF_FFFF) | 1
}

#[cfg(test)]
mod tests;
