//! The local object store, and the remote it exchanges objects with.
//!
//! The store keeps Tresse's objects under `.tresse/meta` and
//! `.tresse/content`, one file per object, named by its hash — the same
//! layout the CLI and the other clients use, now reached through the vault
//! instead of `std::fs`. Whatever a sync carries stays ciphertext: sealing
//! and the blinded keys are upstream's (`rdx-sync`).

use std::sync::Arc;

use rdx_rs::Stamp;
use rdx_sync::{
    ChunkClass, ChunkId, ObjectStore, RemoteKv, SyncError, chunk_id_bytes, verify_object_bytes,
};
use rdx_sync_s3::{Remote, RemoteSpec, S3Config};
use tresse_lib::VersionLabel;
use tresse_lib::commit::{MergedMeta, merge_local_meta};
use tresse_lib::entry::Entry;
use tresse_lib::meta_bundle::{self, MetaBundleError};

use crate::Repo;
use crate::config::{RemoteConfig, StorageType};
use crate::storage::parse_version_label;
use crate::vault::Vault;

pub const DEFAULT_CHUNK_RAW_SIZE: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_OBJECT_SIZE: usize = 96 * 1024 * 1024;

/// Where the local objects live, relative to the repository root.
const STORE_DIR: &str = ".tresse";

/// The local object store, over the repository's vault.
pub struct VaultObjectStore {
    vault: Arc<dyn Vault>,
}

impl VaultObjectStore {
    pub fn open(vault: Arc<dyn Vault>) -> Result<Self, SyncError> {
        vault
            .mkdir_all(&format!("{STORE_DIR}/{}", ChunkClass::Meta.prefix()))
            .map_err(SyncError::Io)?;
        vault
            .mkdir_all(&format!("{STORE_DIR}/{}", ChunkClass::Content.prefix()))
            .map_err(SyncError::Io)?;
        Ok(Self { vault })
    }

    fn class_dir(&self, class: ChunkClass) -> String {
        format!("{STORE_DIR}/{}", class.prefix())
    }

    fn object_path(&self, class: ChunkClass, id: &ChunkId) -> String {
        format!("{}/{}", self.class_dir(class), id)
    }
}

impl ObjectStore for VaultObjectStore {
    fn list_objects(&self, class: ChunkClass) -> Result<Vec<ChunkId>, SyncError> {
        let entries = self
            .vault
            .list(&self.class_dir(class))
            .map_err(SyncError::Io)?;
        Ok(entries
            .into_iter()
            .filter(|entry| !entry.is_dir && is_chunk_filename(&entry.name))
            .map(|entry| entry.name)
            .collect())
    }

    fn get_bytes(&self, class: ChunkClass, id: &ChunkId) -> Result<Vec<u8>, SyncError> {
        let bytes = match self.vault.read(&self.object_path(class, id)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SyncError::NotFound(id.clone()));
            }
            Err(error) => return Err(SyncError::Io(error)),
        };
        verify_object_bytes(id, &bytes)?;
        Ok(bytes)
    }

    fn put_bytes(&mut self, class: ChunkClass, bytes: &[u8]) -> Result<ChunkId, SyncError> {
        let id = chunk_id_bytes(bytes);
        let path = self.object_path(class, &id);
        if self.vault.metadata(&path).map_err(SyncError::Io)?.is_none() {
            self.vault.write(&path, bytes).map_err(SyncError::Io)?;
        }
        Ok(id)
    }

    fn delete_object(&mut self, class: ChunkClass, id: &ChunkId) -> Result<(), SyncError> {
        self.vault
            .remove_file(&self.object_path(class, id))
            .map_err(SyncError::Io)
    }
}

/// A chunk filename is exactly a 64-char lowercase blake3 hex digest. Anything
/// else in the directory (staged writes, the user's own files) is ignored.
fn is_chunk_filename(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn open_local_store(repo: &Repo) -> Result<VaultObjectStore, SyncError> {
    VaultObjectStore::open(repo.vault().clone())
}

pub fn load_merged_meta(store: &VaultObjectStore) -> Result<MergedMeta, MetaBundleError> {
    Ok(merge_local_meta(load_meta_entries(store)?)?)
}

/// Записи меты до слияния — так их читает домен показа (`tresse-history`).
pub fn load_meta_entries(store: &VaultObjectStore) -> Result<Vec<Entry>, MetaBundleError> {
    let objects = meta_bundle::load_meta_objects(store)?;
    Ok(meta_bundle::assemble_meta(&objects).entries)
}

/// Метка версии из аргумента → штамп из канонического списка.
///
/// Метка — не адрес: собрать `source` обратно нельзя, сверяются метки.
pub fn resolve_version(meta: &MergedMeta, spec: &str) -> Result<Stamp, String> {
    let label = parse_version_label(spec)?;
    tresse_lib::versioning::find_version_by_label(&meta.versions, label).ok_or_else(|| {
        let available = meta
            .versions
            .iter()
            .map(|stamp| VersionLabel::from_source(stamp.source).format())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "version {} not found (available: {available})",
            label.format()
        )
    })
}

/// The configured origin, opened. One `Remote` type speaks both the server
/// protocol (presigned URLs) and plain S3; the encryption key is validated
/// here, before the first remote call, not when the first object is sealed.
pub struct ConfiguredRemote {
    inner: Remote,
}

impl ConfiguredRemote {
    pub fn open(config: &RemoteConfig) -> Result<Self, SyncError> {
        tresse_lib::crypto::decode_key_b64(&config.encryption_key)
            .map_err(|error| SyncError::Other(error.to_string()))?;
        let spec = match config.storage_type {
            // The server finds the repository by its token; `repo_id` is a
            // local label only.
            StorageType::Http => RemoteSpec::Server {
                url: config.url.clone(),
                token: config.token.clone(),
            },
            StorageType::S3 => RemoteSpec::S3(S3Config {
                endpoint: config.s3_endpoint.clone(),
                region: config.s3_region.clone(),
                bucket: config.s3_bucket.clone(),
                access_key_id: config.s3_access_key_id.clone(),
                secret_access_key: config.s3_secret_access_key.clone(),
                prefix: config.repo_id.clone(),
            }),
        };
        Ok(Self {
            inner: Remote::open(spec)?,
        })
    }
}

impl RemoteKv for ConfiguredRemote {
    fn list(&self, prefix: &str) -> Result<Vec<String>, SyncError> {
        self.inner.list(prefix)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, SyncError> {
        self.inner.get(key)
    }

    fn put(&mut self, key: &str, value: &[u8]) -> Result<(), SyncError> {
        self.inner.put(key, value)
    }

    fn delete(&mut self, key: &str) -> Result<(), SyncError> {
        self.inner.delete(key)
    }
}

pub struct EmptyRemote;

impl ObjectStore for EmptyRemote {
    fn list_objects(&self, _class: ChunkClass) -> Result<Vec<String>, SyncError> {
        Ok(Vec::new())
    }

    fn get_bytes(&self, _class: ChunkClass, id: &String) -> Result<Vec<u8>, SyncError> {
        Err(SyncError::NotFound(id.clone()))
    }

    fn put_bytes(&mut self, class: ChunkClass, bytes: &[u8]) -> Result<String, SyncError> {
        let _ = (class, bytes);
        Err(SyncError::Other("empty remote is read-only".to_string()))
    }
}
