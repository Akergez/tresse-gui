//! Shared `tresse.toml` and local `.tresse/remotes.toml` configuration parsing.
//!
//! Both files are read and written through the vault; neither is ever cached
//! in the application's settings, so a change another client makes is seen by
//! the next operation.

use serde::{Deserialize, Serialize};
use std::io;
use tresse_lib::{IgnoreRules, LanguageMap};

use crate::Repo;
use crate::vault::Vault;

/// Device-local remote configuration, relative to the repository root.
pub const REMOTES_PATH: &str = ".tresse/remotes.toml";

/// The repository's own configuration, versioned like any other file.
pub const CONFIG_PATH: &str = "tresse.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TresseConfig {
    #[serde(default)]
    pub ignore: IgnoreConfig,
    #[serde(default)]
    pub languages: LanguageMap,
    #[serde(skip)]
    pub remotes: Vec<RemoteConfig>,
}

/// Образцов по умолчанию нет и быть не может: список исключений задаёт конфиг
/// и только он. Клиент, подставляющий свои строки, когда в `tresse.toml` их
/// нет, подменяет собой автора репозитория — тот прочтёт файл, увидит один
/// набор правил, а синхронизация пойдёт по другому. Пустой список означает
/// ровно то, что написано: не исключается ничего, кроме `.tresse/`, который
/// матчер закрывает сам как своё же хранилище.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IgnoreConfig {
    #[serde(default)]
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    #[serde(default)]
    pub storage_type: StorageType,
    pub url: String,
    pub token: String,
    pub repo_id: String,
    /// Base64-encoded 32-byte encryption key for E2E encryption.
    pub encryption_key: String,
    #[serde(default)]
    pub s3_endpoint: String,
    #[serde(default)]
    pub s3_region: String,
    #[serde(default)]
    pub s3_bucket: String,
    #[serde(default)]
    pub s3_access_key_id: String,
    #[serde(default)]
    pub s3_secret_access_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageType {
    #[default]
    Http,
    S3,
}

impl RemoteConfig {
    /// Записать этот remote единственным `origin` в `.tresse/remotes.toml`,
    /// заменив прежний файл целиком.
    ///
    /// Через временный файл и переименование: в файле лежит ключ шифрования,
    /// и оборванная запись не должна оставить на его месте обрубок. Внутри
    /// `.tresse/` vault пишет файлы доступными только владельцу.
    pub fn write_origin(&self, vault: &dyn Vault) -> io::Result<()> {
        let local = LocalConfig {
            remotes: RemotesConfig {
                origin: vec![self.clone()],
            },
        };
        let text = toml::to_string_pretty(&local).map_err(|e| io::Error::other(e.to_string()))?;
        vault.write(REMOTES_PATH, text.as_bytes())?;
        Ok(())
    }
}

impl TresseConfig {
    /// Load shared config and device-local remotes.
    pub fn load(vault: &dyn Vault) -> io::Result<Self> {
        let mut config = match vault.read(CONFIG_PATH) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes)
                    .map_err(|e| io::Error::other(format!("tresse.toml is not UTF-8: {e}")))?;
                toml::from_str(&text).map_err(|e| io::Error::other(e.to_string()))?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        config
            .languages
            .validate()
            .map_err(|e| io::Error::other(e.to_string()))?;
        config.remotes = LocalConfig::load(vault)?.remotes.origin;
        Ok(config)
    }

    pub fn origin(&self) -> Option<&RemoteConfig> {
        self.remotes.first()
    }

    /// Разбирает правила исключения и сообщает о мёртвых отрицаниях: молчать о
    /// них хуже, чем шуметь, — пользователь считает путь возвращённым, а его
    /// нет.
    pub fn ignore_rules(&self) -> io::Result<IgnoreRules> {
        let rules = IgnoreRules::compile(&self.ignore.patterns)
            .map_err(|error| io::Error::other(error.to_string()))?;
        for warning in rules.warnings() {
            eprintln!("warning: {warning}");
        }
        Ok(rules)
    }

    /// Завести пустой `tresse.toml` в корне. Ошибка, если файл уже есть.
    ///
    /// Пустой — значит с пустым `patterns`: заготовка показывает, где писать
    /// правила, но не решает за автора, какие. Наполняет её он.
    pub fn init(vault: &dyn Vault) -> io::Result<()> {
        if vault.metadata(CONFIG_PATH)?.is_some() {
            return Err(io::Error::other("tresse.toml already exists"));
        }
        let default = Self::default();
        let text = toml::to_string_pretty(&default).map_err(|e| io::Error::other(e.to_string()))?;
        vault.write(CONFIG_PATH, text.as_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LocalConfig {
    #[serde(default)]
    remotes: RemotesConfig,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RemotesConfig {
    #[serde(default)]
    origin: Vec<RemoteConfig>,
}

impl LocalConfig {
    fn load(vault: &dyn Vault) -> io::Result<Self> {
        let bytes = match vault.read(REMOTES_PATH) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let text = String::from_utf8(bytes)
            .map_err(|e| io::Error::other(format!("remotes.toml is not UTF-8: {e}")))?;
        let local: Self = toml::from_str(&text).map_err(|e| io::Error::other(e.to_string()))?;
        if local.remotes.origin.len() > 1 {
            return Err(io::Error::other(
                "only one `remotes.origin` entry is supported",
            ));
        }
        Ok(local)
    }
}

impl RemoteConfig {
    pub fn to_share_string(&self) -> String {
        crate::config_share::encode(self)
    }

    pub fn from_share_string(input: &str) -> Result<Self, String> {
        crate::config_share::decode(input).map_err(|e| e.to_string())
    }

    pub fn validate(&self) -> Result<(), String> {
        let required = match self.storage_type {
            StorageType::S3 => vec![
                ("Endpoint", &self.s3_endpoint),
                ("Bucket", &self.s3_bucket),
                ("Access key ID", &self.s3_access_key_id),
                ("Secret access key", &self.s3_secret_access_key),
            ],
            StorageType::Http => vec![("Server URL", &self.url)],
        };
        for (name, value) in required
            .into_iter()
            .chain([("Repository ID", &self.repo_id)])
        {
            if value.trim().is_empty() {
                return Err(format!("{name} is required."));
            }
        }
        let endpoint = match self.storage_type {
            StorageType::S3 => &self.s3_endpoint,
            StorageType::Http => &self.url,
        };
        if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
            return Err("Endpoint must start with https:// or http://.".into());
        }
        tresse_lib::crypto::decode_key_b64(&self.encryption_key)
            .map_err(|_| "Encryption key must be 32 bytes in base64.".to_string())?;
        Ok(())
    }
}

/// Read the configured origin each time; never cache credentials in app settings.
pub fn read_remote(vault: &dyn Vault) -> Result<Option<RemoteConfig>, String> {
    TresseConfig::load(vault)
        .map(|config| config.origin().cloned())
        .map_err(|e| e.to_string())
}

/// Write (or, with `None`, remove) the origin under the repository lock.
pub fn write_remote(repo: &Repo, remote: Option<&RemoteConfig>) -> Result<(), String> {
    let _lock = crate::operation::RepoLock::acquire(repo).map_err(|e| e.to_string())?;
    match remote {
        Some(remote) => {
            remote.validate()?;
            remote
                .write_origin(repo.vault().as_ref())
                .map_err(|e| e.to_string())
        }
        None => repo
            .vault()
            .remove_file(REMOTES_PATH)
            .map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{RemoteConfig, StorageType, TresseConfig};
    use crate::vault::{FsVault, Vault};
    use std::fs;

    fn vault() -> (tempfile::TempDir, FsVault) {
        let directory = tempfile::tempdir().unwrap();
        let vault = FsVault::open(directory.path()).unwrap();
        (directory, vault)
    }

    #[test]
    fn load_reads_origin_from_local_remotes_file() {
        let (_directory, vault) = vault();
        vault
            .write(
                ".tresse/remotes.toml",
                br#"
[[remotes.origin]]
storage_type = "s3"
url = ""
token = ""
repo_id = "vault"
encryption_key = "key"
s3_endpoint = "https://s3.example.com"
s3_region = "us-east-1"
s3_bucket = "bucket"
s3_access_key_id = "access"
s3_secret_access_key = "secret"
"#,
            )
            .unwrap();

        let config = TresseConfig::load(&vault).unwrap();
        assert!(matches!(
            config.origin().unwrap().storage_type,
            StorageType::S3
        ));
    }

    #[test]
    fn a_written_origin_is_what_load_reads_back() {
        let (_directory, vault) = vault();
        vault
            .write(".tresse/remotes.toml", b"stale = true\n")
            .unwrap();
        let remote = RemoteConfig {
            storage_type: StorageType::S3,
            url: String::new(),
            token: String::new(),
            repo_id: "vault".into(),
            encryption_key: "a+b/c==".into(),
            s3_endpoint: "https://s3.example.com".into(),
            s3_region: "us-east-1".into(),
            s3_bucket: "bucket".into(),
            s3_access_key_id: "access".into(),
            s3_secret_access_key: "sec\"ret".into(),
        };

        remote.write_origin(&vault).unwrap();

        let config = TresseConfig::load(&vault).unwrap();
        assert_eq!(config.origin(), Some(&remote));
    }

    #[test]
    fn a_failed_write_leaves_no_staged_credentials_behind() {
        let (directory, vault) = vault();
        // Каталог на месте файла: переименование поверх него не пройдёт.
        fs::create_dir_all(directory.path().join(".tresse/remotes.toml")).unwrap();
        let remote: RemoteConfig =
            toml::from_str("url = \"\"\ntoken = \"\"\nrepo_id = \"\"\nencryption_key = \"k\"\n")
                .unwrap();

        assert!(remote.write_origin(&vault).is_err());

        let staged = fs::read_dir(directory.path().join(".tresse/tmp"));
        let leftovers = staged.map(|entries| entries.count()).unwrap_or(0);
        assert_eq!(leftovers, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_written_origin_is_readable_by_the_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let (directory, vault) = vault();
        let remote: RemoteConfig =
            toml::from_str("url = \"\"\ntoken = \"\"\nrepo_id = \"\"\nencryption_key = \"\"\n")
                .unwrap();

        remote.write_origin(&vault).unwrap();

        let mode = fs::metadata(directory.path().join(".tresse/remotes.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn without_a_config_nothing_is_ignored_but_the_layout_directory() {
        let rules = TresseConfig::default().ignore_rules().unwrap();

        assert!(TresseConfig::default().ignore.patterns.is_empty());
        assert!(rules.warnings().is_empty());
        // Скрытое, чужие хранилища, корзина — всё это исключает конфиг, если
        // так решил автор репозитория. Клиент за него не решает.
        assert!(!rules.is_ignored(".git", true));
        assert!(!rules.is_ignored(".trash/deleted.md", false));
        assert!(!rules.is_ignored("notes/.DS_Store", false));
        assert!(!rules.is_ignored("notes/visible.md", false));
        // Общая настройка репозитория лежит в корне и синхронизируется.
        assert!(!rules.is_ignored("tresse.toml", false));
        // Своё же хранилище матчер закрывает сам, без единого образца.
        assert!(rules.is_ignored(".tresse", true));
    }

    #[test]
    fn a_config_without_an_ignore_section_ignores_nothing() {
        let (_directory, vault) = vault();
        vault.write("tresse.toml", b"[languages]\n").unwrap();

        let config = TresseConfig::load(&vault).unwrap();

        assert!(config.ignore.patterns.is_empty());
    }

    #[test]
    fn init_writes_config_at_project_root() {
        let (_directory, vault) = vault();

        TresseConfig::init(&vault).unwrap();

        let text = String::from_utf8(vault.read("tresse.toml").unwrap()).unwrap();
        assert!(text.contains("[languages.markdown]"));
        assert!(text.contains("[languages.rdx]"));
        assert!(text.contains("strategy = \"native\""));
    }

    #[test]
    fn load_reads_shared_config_from_project_root() {
        let (_directory, vault) = vault();
        vault
            .write("tresse.toml", b"[ignore]\npatterns = [\"root\"]\n")
            .unwrap();

        let config = TresseConfig::load(&vault).unwrap();

        assert_eq!(config.ignore.patterns, ["root"]);
    }

    #[test]
    fn load_rejects_non_origin_remote() {
        let (_directory, vault) = vault();
        vault
            .write(
                ".tresse/remotes.toml",
                b"[[remotes.backup]]\nurl = \"\"\ntoken = \"\"\nrepo_id = \"\"\nencryption_key = \"\"\n",
            )
            .unwrap();

        let error = TresseConfig::load(&vault).unwrap_err();

        assert!(error.to_string().contains("unknown field `backup`"));
    }

    #[test]
    fn load_rejects_multiple_remotes() {
        let (_directory, vault) = vault();
        let remote =
            "[[remotes.origin]]\nurl = \"\"\ntoken = \"\"\nrepo_id = \"\"\nencryption_key = \"\"\n";
        vault
            .write(
                ".tresse/remotes.toml",
                format!("{remote}{remote}").as_bytes(),
            )
            .unwrap();

        let error = TresseConfig::load(&vault).unwrap_err();

        assert_eq!(
            error.to_string(),
            "only one `remotes.origin` entry is supported"
        );
    }

    #[test]
    fn load_rejects_legacy_remote_in_shared_config() {
        let (_directory, vault) = vault();
        vault
            .write("tresse.toml", b"[remote]\nurl = \"https://example.com\"\n")
            .unwrap();

        let error = TresseConfig::load(&vault).unwrap_err();

        assert!(error.to_string().contains("unknown field `remote`"));
    }

    #[test]
    fn load_rejects_legacy_remote_in_local_config() {
        let (_directory, vault) = vault();
        vault
            .write(
                ".tresse/remotes.toml",
                b"[remote]\nurl = \"https://example.com\"\n",
            )
            .unwrap();

        let error = TresseConfig::load(&vault).unwrap_err();

        assert!(error.to_string().contains("unknown field `remote`"));
    }
}
