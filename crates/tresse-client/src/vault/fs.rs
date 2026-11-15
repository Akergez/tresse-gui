//! The desktop vault: ordinary paths through `std::fs`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use super::{Entry, Meta, Vault, VaultLock, Written, is_plain};
use crate::os_lock::try_lock;

/// The directory a write is staged in before it is moved into place.
const STAGING_DIR: &str = ".tresse/tmp";

pub struct FsVault {
    root: PathBuf,
    staged: AtomicU64,
}

impl FsVault {
    /// A vault over `root`, created if missing.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            staged: AtomicU64::new(0),
        })
    }

    fn path(&self, path: &str) -> io::Result<PathBuf> {
        if !is_plain(path) {
            return Err(io::Error::other(format!("unsafe path: {path}")));
        }
        Ok(self.root.join(path))
    }

    fn dir_path(&self, dir: &str) -> io::Result<PathBuf> {
        if dir.is_empty() {
            return Ok(self.root.clone());
        }
        self.path(dir)
    }

    /// Write `bytes` to a file in the staging area and hand back where it is
    /// and what it says about itself.
    ///
    /// Files inside `.tresse/` are written owner-only: the remote
    /// configuration carries the encryption key, and the journal and index
    /// are local state nobody else has business reading.
    fn stage(&self, path: &str, bytes: &[u8]) -> io::Result<(PathBuf, Written)> {
        let directory = self.root.join(STAGING_DIR);
        fs::create_dir_all(&directory)?;
        let number = self.staged.fetch_add(1, Ordering::Relaxed);
        let staged = directory.join(format!("{}-{number}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let owner_only = path.starts_with(".tresse/");
        #[cfg(unix)]
        if owner_only {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let _ = owner_only;
        let mut file = options.open(&staged)?;
        file.write_all(bytes)?;
        // A storage that cannot sync a file (a FUSE mount, a phone's picked
        // tree) still takes the write; only a real failure is one.
        if let Err(error) = file.sync_all()
            && error.kind() != io::ErrorKind::Unsupported
        {
            return Err(error);
        }
        drop(file);
        let metadata = fs::metadata(&staged)?;
        Ok((
            staged,
            Written {
                size: metadata.len(),
                mtime_ms: mtime_ms(&metadata),
            },
        ))
    }
}

impl Vault for FsVault {
    fn list(&self, dir: &str) -> io::Result<Vec<Entry>> {
        let directory = self.dir_path(dir)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            // Follows a link the way a walk follows it; a link to nothing is
            // not a file of this tree.
            let Ok(metadata) = fs::metadata(entry.path()) else {
                continue;
            };
            entries.push(Entry {
                name,
                is_dir: metadata.is_dir(),
                len: metadata.len(),
                mtime_ms: mtime_ms(&metadata),
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn metadata(&self, path: &str) -> io::Result<Option<Meta>> {
        let target = self.path(path)?;
        match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                let is_dir = metadata.is_dir();
                Ok(Some(Meta {
                    is_dir,
                    len: if is_dir { 0 } else { metadata.len() },
                    mtime_ms: mtime_ms(&metadata),
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.path(path)?)
    }

    fn read_range(&self, path: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut file = File::open(self.path(path)?)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0u8; len];
        let mut read = 0;
        while read < len {
            let count = file.read(&mut bytes[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        bytes.truncate(read);
        Ok(bytes)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> io::Result<Written> {
        let target = self.path(path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let (staged, written) = self.stage(path, bytes)?;
        match fs::rename(&staged, &target) {
            Ok(()) => Ok(written),
            Err(error) => {
                let _ = fs::remove_file(&staged);
                Err(error)
            }
        }
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        match fs::remove_file(self.path(path)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        fs::remove_dir(self.path(path)?)
    }

    fn remove_dir_all(&self, path: &str) -> io::Result<()> {
        match fs::remove_dir_all(self.path(path)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn mkdir_all(&self, dir: &str) -> io::Result<()> {
        if dir.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(self.path(dir)?)
    }

    fn lock(&self) -> io::Result<Box<dyn VaultLock>> {
        let path = self.root.join(tresse_lib::operation::OS_LOCK_PATH);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match try_lock(&file) {
            Ok(()) => Ok(Box::new(FsLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another process holds the repository lock",
            )),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

struct FsLock {
    _file: File,
}

impl VaultLock for FsLock {}

fn mtime_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, FsVault) {
        let directory = tempfile::tempdir().unwrap();
        let vault = FsVault::open(directory.path()).unwrap();
        (directory, vault)
    }

    #[test]
    fn a_written_file_reads_back_and_lists_its_folder() {
        let (directory, vault) = vault();
        let written = vault.write("notes/a.md", b"hello").unwrap();
        assert_eq!(written.size, 5);
        assert_eq!(vault.read("notes/a.md").unwrap(), b"hello");
        assert_eq!(vault.read_range("notes/a.md", 1, 3).unwrap(), b"ell");
        let root = vault.list("").unwrap();
        // `.tresse` is made by the write itself: that is where staging lives.
        let names: Vec<&str> = root.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, [".tresse", "notes"]);
        assert!(root[0].is_dir);
        assert!(root[1].is_dir);
        let notes = vault.list("notes").unwrap();
        assert_eq!(notes[0].name, "a.md");
        assert_eq!(notes[0].len, 5);
        assert_eq!(vault.metadata("notes/a.md").unwrap().unwrap().len, 5);
        assert!(vault.metadata("notes/b.md").unwrap().is_none());
        drop(directory);
    }

    #[test]
    fn a_written_file_replaces_the_old_one_in_one_step() {
        let (_directory, vault) = vault();
        vault.write("a.txt", b"first").unwrap();
        vault.write("a.txt", b"second").unwrap();
        assert_eq!(vault.read("a.txt").unwrap(), b"second");
    }

    #[test]
    fn removing_what_is_not_there_is_not_an_error() {
        let (_directory, vault) = vault();
        vault.remove_file("missing").unwrap();
        vault.remove_dir_all("missing").unwrap();
    }

    #[test]
    fn a_path_never_leaves_the_root() {
        let (_directory, vault) = vault();
        assert!(vault.write("../outside", b"no").is_err());
        assert!(vault.read("a/../../b").is_err());
    }

    #[test]
    fn the_lock_admits_one_holder_and_is_released_when_dropped() {
        let (_directory, vault) = vault();
        let held = vault.lock().unwrap();
        let error = vault.lock().err().expect("the second lock must fail");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(held);
        vault.lock().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_remote_configuration_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, vault) = vault();
        vault.write(".tresse/remotes.toml", b"secret").unwrap();
        let mode = fs::metadata(directory.path().join(".tresse/remotes.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
