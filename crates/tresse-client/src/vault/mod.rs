//! The filesystem seam.
//!
//! Everything the client does with a folder goes through [`Vault`]: the walk
//! that builds a commit's shape, the files a restore writes, the local object
//! store, the operation journal and the repository lock. On a desktop that is
//! [`fs::FsVault`] over ordinary paths; on a phone it will be a vault over
//! Android's storage access framework, where there are no paths to hand to
//! `std::fs`.
//!
//! Paths are root-relative and use `/`, never a leading one. A path is either
//! `""` — the repository root, for [`Vault::list`] only — or one or more
//! plain components.

pub mod fs;

pub use fs::FsVault;

use std::io;

/// A file or directory named in [`Vault::list`].
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub len: u64,
    pub mtime_ms: u64,
}

/// What [`Vault::metadata`] says about one path.
#[derive(Debug, Clone, Copy)]
pub struct Meta {
    pub is_dir: bool,
    pub len: u64,
    pub mtime_ms: u64,
}

/// A file as [`Vault::write`] left it, taken before it was put in place: a
/// change made right after the write must still look like a change.
#[derive(Debug, Clone, Copy)]
pub struct Written {
    pub size: u64,
    pub mtime_ms: u64,
}

/// The OS lock on a repository, held until the handle is dropped. The kernel
/// releases it when the process dies, so no lock ever outlives its owner.
pub trait VaultLock: Send {}

/// A folder, reached through a seam.
///
/// Implementations live in this crate ([`fs::FsVault`]) and in the phone
/// build (the storage access framework); the client talks to this trait only.
pub trait Vault: Send + Sync {
    /// The direct children of `dir` (`""` is the root), sorted by name.
    /// Symlinks are followed the way a directory walk follows them.
    fn list(&self, dir: &str) -> io::Result<Vec<Entry>>;

    /// One path's metadata, not following a symlink: a link is not the file
    /// it points at, and a restore must never take one for the other.
    /// `Ok(None)` is "there is nothing there".
    fn metadata(&self, path: &str) -> io::Result<Option<Meta>>;

    /// A file's whole content.
    fn read(&self, path: &str) -> io::Result<Vec<u8>>;

    /// Up to `len` bytes of a file, from `offset`. Fewer bytes mean the file
    /// ended first.
    fn read_range(&self, path: &str, offset: u64, len: usize) -> io::Result<Vec<u8>>;

    /// Replace a file's content in one step: written beside it and moved into
    /// place, so an interrupted write leaves either the old file or the new
    /// one. The returned size and time are those of the written file, taken
    /// before it took its place.
    fn write(&self, path: &str, bytes: &[u8]) -> io::Result<Written>;

    /// Remove a file; nothing there is not an error.
    fn remove_file(&self, path: &str) -> io::Result<()>;

    /// Remove a directory that is already empty.
    fn remove_dir(&self, path: &str) -> io::Result<()>;

    /// Remove a directory and everything under it; nothing there is not an
    /// error. For the staging area of an interrupted operation.
    fn remove_dir_all(&self, path: &str) -> io::Result<()>;

    fn mkdir_all(&self, dir: &str) -> io::Result<()>;

    /// Take the repository's OS lock, or fail with
    /// [`io::ErrorKind::WouldBlock`] when another process holds it.
    fn lock(&self) -> io::Result<Box<dyn VaultLock>>;
}

/// `path` is root-relative, non-empty, and made of plain components only:
/// no `..`, no `.`, no empty part.
pub(crate) fn is_plain(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// `dir`/`name`, with the root written as `name`.
pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The directory `path` is in; `None` for a name in the root.
pub(crate) fn parent_of(path: &str) -> Option<&str> {
    path.rfind('/').map(|slash| &path[..slash])
}
