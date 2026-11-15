//! Building a commit's shape from the worktree, and handing file content to
//! the commit engine.
//!
//! Здесь только внешний мир: обход, правила исключения и определение природы
//! файла. Что из этого следует — какой путь считать нетронутым и как сложить
//! форму — решает [`tresse_lib::file_index::shape_from_files`]. Обход идёт
//! через vault, поэтому одинаково работает с путями десктопа и с деревом,
//! выбранным на телефоне.

use std::sync::Arc;

use tresse_lib::file_index::shape_from_files;
use tresse_lib::{FileIndex, FileIndexEntry, FlatFileSource, IgnoreRules, IndexedFile, Shape};

use crate::vault::{Vault, join};

// ---------------------------------------------------------------------------
// Shape builder (path structure only, no content loaded)
// ---------------------------------------------------------------------------

pub fn build_shape(
    vault: &dyn Vault,
    ignore: &IgnoreRules,
    previous: Option<&FileIndex>,
) -> std::io::Result<(Shape, FileIndex)> {
    let mut files = Vec::new();
    collect(vault, "", ignore, previous, &mut files)?;
    Ok(shape_from_files(files, previous))
}

fn collect(
    vault: &dyn Vault,
    dir: &str,
    ignore: &IgnoreRules,
    previous: Option<&FileIndex>,
    files: &mut Vec<IndexedFile>,
) -> std::io::Result<()> {
    for entry in vault.list(dir)? {
        let child = join(dir, &entry.name);
        if ignore.is_ignored(&child, entry.is_dir) {
            continue;
        }
        if entry.is_dir {
            if let Err(error) = collect(vault, &child, ignore, previous, files) {
                eprintln!("  skipping {child}: {error}");
            }
            continue;
        }
        // Природу файла определяем один раз и запоминаем: перечитывать его
        // целиком ради этого на каждом коммите незачем.
        let binary = previous
            .and_then(|index| index.get(&child))
            .map_or_else(|| is_binary(vault, &child), |entry| entry.binary);
        files.push(IndexedFile::new(
            &child,
            FileIndexEntry {
                mtime_ms: entry.mtime_ms,
                size: entry.len,
                binary,
            },
        ));
        if files.len() % 1000 == 0 {
            eprintln!("scanning the folder: {} files…", files.len());
        }
    }
    Ok(())
}

/// A file that does not start as UTF-8 is a blob. A file that cannot be read
/// at all is taken for one too: a commit must not claim to have parsed it.
///
/// Only the beginning is read. A phone's storage answers each read with a
/// binder round trip, and reading a whole video to find out that it is not
/// text is what made a first commit on a big folder look hung. A truncated
/// multi-byte character at the sample's edge is text, not a blob.
const SAMPLE: usize = 64 * 1024;

fn is_binary(vault: &dyn Vault, path: &str) -> bool {
    match vault.read_range(path, 0, SAMPLE) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(_) => false,
            Err(error) => error.error_len().is_some(),
        },
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------------
// FlatFileSource backed by the vault
// ---------------------------------------------------------------------------

pub struct VaultSource {
    vault: Arc<dyn Vault>,
}

impl VaultSource {
    pub fn new(vault: Arc<dyn Vault>) -> Self {
        Self { vault }
    }
}

impl FlatFileSource for VaultSource {
    fn len(&self, path: &str) -> u64 {
        self.vault
            .metadata(path)
            .ok()
            .flatten()
            .map_or(0, |meta| meta.len)
    }

    fn read(&self, path: &str, offset: u64, len: usize) -> Vec<u8> {
        self.vault.read_range(path, offset, len).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::FsVault;

    #[test]
    fn missing_index_scans_all_files_then_matching_index_marks_them_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let vault = FsVault::open(directory.path()).unwrap();
        vault.write("a.md", b"hello").unwrap();

        let (first, index) = build_shape(&vault, &IgnoreRules::default(), None).unwrap();
        let Shape::Dir { children, .. } = first else {
            panic!()
        };
        assert!(matches!(children.as_slice(), [Shape::File { path }] if path == "a.md"));

        let (second, _) = build_shape(&vault, &IgnoreRules::default(), Some(&index)).unwrap();
        let Shape::Dir { children, .. } = second else {
            panic!()
        };
        assert!(
            matches!(children.as_slice(), [Shape::Unchanged { path, binary: false }] if path == "a.md")
        );
    }

    #[test]
    fn tresse_directory_is_always_ignored() {
        assert!(IgnoreRules::default().is_ignored(".tresse", true));
    }
}
