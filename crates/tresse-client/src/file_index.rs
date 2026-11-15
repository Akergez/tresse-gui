//! Файловая обвязка над общим индексом.
//!
//! Здесь только то, что знает про хранилище: как спросить у vault время и
//! размер и где лежит сам индекс. Состав записи, правило сравнения и формат
//! файла принадлежат домену — [`tresse_lib::file_index`].

use std::io;

use tresse_lib::file_index::PATH;
use tresse_lib::{FileIndex, FileIndexEntry, IndexedFile};

use crate::restore_sink::WrittenFile;
use crate::vault::Vault;

/// Прочитать индекс, если он есть и читается.
///
/// Битый или отсутствующий индекс — не ошибка коммита: без него просто не будет
/// оптимизации, и все файлы прочитаются заново.
pub fn load(vault: &dyn Vault) -> Option<FileIndex> {
    FileIndex::from_json(&vault.read(PATH).ok()?).ok()
}

pub fn save(vault: &dyn Vault, index: &FileIndex) -> Result<(), Box<dyn std::error::Error>> {
    vault.write(PATH, &index.to_json()?)?;
    Ok(())
}

/// Подновить индекс после восстановления файлов в рабочую директорию.
///
/// Индекса могло ещё не быть — тогда он заводится здесь: по нему restore потом
/// узнаёт свои же файлы, и без записи о них реплика, которая только тянет,
/// приняла бы их за чужие.
///
/// Время и размер берутся те, с какими файл был записан, а не те, что в vault
/// сейчас: правка, сделанная сразу после записи, должна остаться заметной.
pub fn apply_restore(
    vault: &dyn Vault,
    written: &[WrittenFile],
    removed: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if written.is_empty() && removed.is_empty() {
        return Ok(());
    }
    let mut index = load(vault).unwrap_or_default();
    let mut restored = Vec::with_capacity(written.len());
    for file in written {
        // Блоб ещё не значит «бинарный»: текст без парсера тоже идёт блобом, а
        // природу файла индекс помнит такой, какой её увидел бы обход.
        let binary = file.blob
            && match index.get(&file.path) {
                Some(known) => known.binary,
                None => match is_utf8(vault, &file.path) {
                    Ok(text) => !text,
                    Err(_) => continue,
                },
            };
        restored.push(IndexedFile::new(
            file.path.clone(),
            FileIndexEntry {
                mtime_ms: file.mtime_ms,
                size: file.size,
                binary,
            },
        ));
    }
    index.apply_restored(restored, removed.iter().map(String::as_str));
    save(vault, &index)
}

/// Читается ли файл как UTF-8. Кусками: блоб целиком в память не поднимается.
fn is_utf8(vault: &dyn Vault, path: &str) -> io::Result<bool> {
    const CHUNK: usize = 64 * 1024;
    let mut offset = 0u64;
    // Хвост символа, разрезанного границей куска.
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let chunk = vault.read_range(path, offset, CHUNK)?;
        if chunk.is_empty() {
            return Ok(pending.is_empty());
        }
        offset += chunk.len() as u64;
        pending.extend_from_slice(&chunk);
        match std::str::from_utf8(&pending) {
            Ok(_) => pending.clear(),
            Err(error) if error.error_len().is_some() => return Ok(false),
            Err(error) => {
                pending.drain(..error.valid_up_to());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_utf8;
    use crate::vault::{FsVault, Vault};

    #[test]
    fn a_character_split_across_chunks_still_reads_as_text() {
        let directory = tempfile::tempdir().unwrap();
        let vault = FsVault::open(directory.path()).unwrap();
        let mut text = "a".repeat(64 * 1024 - 1);
        text.push('ж');
        vault.write("text", text.as_bytes()).unwrap();
        assert!(is_utf8(&vault, "text").unwrap());

        vault.write("text", &[b'a', 0xff, b'b']).unwrap();
        assert!(!is_utf8(&vault, "text").unwrap());

        vault.write("text", &text.as_bytes()[..64 * 1024]).unwrap();
        assert!(!is_utf8(&vault, "text").unwrap());
    }
}
