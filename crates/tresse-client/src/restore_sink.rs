//! Выход restore в рабочую директорию: куда и при каких условиях клиент
//! кладёт файлы.
//!
//! Алгоритм restore живёт в домене и про хранилище не знает; здесь то, что
//! знает: запись через vault с переименованием на место, охрана файлов,
//! изменённых пользователем, и учёт того, что в директории в итоге изменилось.

use std::io;
use std::sync::Arc;

use tresse_lib::restore::RestoreSink;
use tresse_lib::{FileIndex, IgnoreRules};

use crate::vault::{Vault, is_plain, parent_of};

/// Выход restore в рабочую директорию.
///
/// С охраной ([`VaultRestoreSink::guarded`]) не трогает то, что пользователь
/// изменил после прошлого коммита или restore: такого содержимого нет в
/// истории, и затереть или удалить его значит потерять безвозвратно.
/// Пропущенное остаётся как есть.
///
/// Без охраны ([`VaultRestoreSink::forced`]) приводит директорию к заказанному,
/// что бы в ней ни лежало, — это `tresse restore <версия>`, где перезапись и
/// есть просьба пользователя.
pub struct VaultRestoreSink {
    vault: Arc<dyn Vault>,
    ignore: IgnoreRules,
    guard: Option<FileIndex>,
    written: Vec<WrittenFile>,
    removed: Vec<String>,
    skipped: Vec<String>,
}

/// Файл, который restore положил в директорию, каким он был в момент записи.
///
/// Время и размер сняты до того, как файл встал на место: снятые позже, они
/// выдали бы правку, сделанную пользователем сразу после записи, за нетронутый
/// файл, и следующий restore её затёр бы.
pub struct WrittenFile {
    pub path: String,
    /// Пришёл ли файл блобом. Блоб ещё не значит «бинарный».
    pub blob: bool,
    pub mtime_ms: u64,
    pub size: u64,
}

/// Что restore сделал с директорией на самом деле.
pub struct DiskChanges {
    pub written: Vec<WrittenFile>,
    pub removed: Vec<String>,
    /// Пути, которые охрана не дала тронуть.
    pub skipped: Vec<String>,
}

/// Собираемый блоб: содержание копится и пишется в vault одним шагом.
pub struct VaultBlob {
    bytes: Vec<u8>,
}

impl io::Write for VaultBlob {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Что лежит на месте, куда restore собрался писать.
enum Occupant {
    /// Места никто не занял.
    Nobody,
    /// Файл, который Tresse сам положил и который с тех пор не меняли.
    Ours,
    /// Файл, которого Tresse не клал или который успели изменить.
    Foreign,
    /// Не файл: каталог там, где должен быть файл.
    Obstacle,
}

impl VaultRestoreSink {
    pub fn guarded(vault: Arc<dyn Vault>, ignore: IgnoreRules, index: FileIndex) -> Self {
        Self::new(vault, ignore, Some(index))
    }

    pub fn forced(vault: Arc<dyn Vault>, ignore: IgnoreRules) -> Self {
        Self::new(vault, ignore, None)
    }

    fn new(vault: Arc<dyn Vault>, ignore: IgnoreRules, guard: Option<FileIndex>) -> Self {
        Self {
            vault,
            ignore,
            guard,
            written: Vec::new(),
            removed: Vec::new(),
            skipped: Vec::new(),
        }
    }

    pub fn into_changes(self) -> DiskChanges {
        DiskChanges {
            written: self.written,
            removed: self.removed,
            skipped: self.skipped,
        }
    }

    /// Куда ложится путь. `None` — путь закрыт правилами исключения.
    ///
    /// Закрытый путь restore обходит молча: коммит такой файл в форму не берёт,
    /// и положить его в директорию значит завести то, чего репозиторий здесь не
    /// видит. Ошибкой это быть не может — запись дерева с таким путём законна,
    /// правила у реплик свои.
    fn target<'p>(&self, path: &'p str) -> io::Result<Option<&'p str>> {
        if !is_plain(path) {
            return Err(io::Error::other(format!("unsafe restore path: {path}")));
        }
        // Весь путь, а не только первый его компонент: иначе restore кладёт то,
        // что commit в форму не берёт.
        if self.ignore.is_ignored(path, false) {
            return Ok(None);
        }
        Ok(Some(path))
    }

    fn occupant(&self, path: &str) -> Occupant {
        match self.vault.metadata(path) {
            Ok(None) => Occupant::Nobody,
            Ok(Some(meta)) if !meta.is_dir => match &self.guard {
                Some(index) if index.holds(path, meta.mtime_ms, meta.len) => Occupant::Ours,
                Some(_) => Occupant::Foreign,
                None => Occupant::Ours,
            },
            _ => Occupant::Obstacle,
        }
    }

    /// Освободить место под файл: каталог на его пути или файл на месте каталога.
    ///
    /// Только без охраны. С охраной такая помеха останавливает операцию: молча
    /// обойти её нельзя — путь остался бы в проекции без файла, и следующий
    /// коммит похоронил бы его.
    fn clear_obstacles(&self, target: &str) -> io::Result<()> {
        if self.guard.is_some() {
            return Ok(());
        }
        if let Ok(Some(meta)) = self.vault.metadata(target)
            && meta.is_dir
        {
            self.vault.remove_dir_all(target)?;
        }
        let mut ancestor = parent_of(target);
        while let Some(directory) = ancestor {
            if let Ok(Some(meta)) = self.vault.metadata(directory)
                && !meta.is_dir
            {
                self.vault.remove_file(directory)?;
            }
            ancestor = parent_of(directory);
        }
        Ok(())
    }

    /// Файл на месте уже тот, что нужен: писать нечего, но индекс его запомнит.
    fn accept_in_place(&mut self, path: &str, blob: bool) -> io::Result<()> {
        let meta = self
            .vault
            .metadata(path)?
            .ok_or_else(|| io::Error::other(format!("{path} vanished")))?;
        self.written.push(WrittenFile {
            path: path.to_string(),
            blob,
            mtime_ms: meta.mtime_ms,
            size: meta.len,
        });
        Ok(())
    }

    fn write_atomically(&mut self, path: &str, bytes: &[u8], blob: bool) -> io::Result<()> {
        let Some(path) = self.target(path)? else {
            return Ok(());
        };
        match self.occupant(path) {
            Occupant::Obstacle => return Err(blocked(path, "a directory is in its place")),
            Occupant::Foreign => {
                // Совпало — так выглядит файл, записанный перед самым обрывом.
                if self.vault.read(path).is_ok_and(|current| current == bytes) {
                    return self.accept_in_place(path, blob);
                }
                self.skipped.push(path.to_string());
                return Ok(());
            }
            Occupant::Nobody | Occupant::Ours => {}
        }
        self.clear_obstacles(path)?;
        let written = self
            .vault
            .write(path, bytes)
            .map_err(|error| blocked(path, error))?;
        self.written.push(WrittenFile {
            path: path.to_string(),
            blob,
            mtime_ms: written.mtime_ms,
            size: written.size,
        });
        Ok(())
    }
}

impl RestoreSink for VaultRestoreSink {
    type Blob = VaultBlob;

    fn write_text(&mut self, path: &str, content: &str) -> io::Result<()> {
        self.write_atomically(path, content.as_bytes(), false)
    }

    fn write_binary(&mut self, path: &str, data: &[u8]) -> io::Result<()> {
        self.write_atomically(path, data, true)
    }

    fn begin_blob(&mut self, path: &str) -> io::Result<Self::Blob> {
        self.target(path)?;
        Ok(VaultBlob { bytes: Vec::new() })
    }

    fn finish_blob(&mut self, path: &str, blob: Self::Blob) -> io::Result<()> {
        let Some(path) = self.target(path)? else {
            return Ok(());
        };
        // Решается здесь, а не на входе: блоб пишется долго, и файл на месте
        // могли изменить, пока он шёл.
        match self.occupant(path) {
            Occupant::Obstacle => Err(blocked(path, "a directory is in its place")),
            Occupant::Foreign => {
                if self
                    .vault
                    .read(path)
                    .is_ok_and(|current| current == blob.bytes)
                {
                    return self.accept_in_place(path, true);
                }
                self.skipped.push(path.to_string());
                Ok(())
            }
            Occupant::Nobody | Occupant::Ours => {
                self.clear_obstacles(path)?;
                let written = self
                    .vault
                    .write(path, &blob.bytes)
                    .map_err(|error| blocked(path, error))?;
                self.written.push(WrittenFile {
                    path: path.to_string(),
                    blob: true,
                    mtime_ms: written.mtime_ms,
                    size: written.size,
                });
                Ok(())
            }
        }
    }

    fn remove(&mut self, path: &str) -> io::Result<()> {
        let Some(path) = self.target(path)? else {
            return Ok(());
        };
        match self.occupant(path) {
            Occupant::Foreign => {
                self.skipped.push(path.to_string());
                return Ok(());
            }
            // Файла, который надо убрать, там и нет: на его месте каталог.
            Occupant::Obstacle => {}
            Occupant::Nobody | Occupant::Ours => {
                self.vault.remove_file(path)?;
                self.prune_empty_parents(path);
            }
        }
        self.removed.push(path.to_string());
        Ok(())
    }
}

impl VaultRestoreSink {
    /// Убрать каталоги, опустевшие после удаления файла.
    ///
    /// Пустой каталог Tresse не хранит, а оставленный он занял бы место файла
    /// с тем же именем.
    fn prune_empty_parents(&self, target: &str) {
        let mut ancestor = parent_of(target);
        while let Some(directory) = ancestor {
            if self.vault.remove_dir(directory).is_err() {
                break;
            }
            ancestor = parent_of(directory);
        }
    }
}

/// Ошибка restore, который не смог положить файл туда, где ему место.
fn blocked(path: &str, reason: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!(
        "cannot restore {path}: {reason}; move whatever occupies its place and run the operation again"
    ))
}
