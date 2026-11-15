//! Where one folder syncs: a server's sign-in page, a connection string from
//! another device, or every field by hand.
//!
//! The form edits the remote in the form; Save writes
//! `.tresse/remotes.toml` in the folder, and the sync reads that file again.
//! Nothing about the remote is kept in this application's settings — the
//! key included.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_adaptive::prim::row;
use gpui_adaptive::{
    ActiveScale as _, Command, Fields, Form, Labelled, Note, Notice, Purpose, Segments, Setting,
    TextField, TextFieldState, Tone,
};
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, ClipboardItem, Context, Entity, SharedString, Window, rems};
use tresse_client::{KeyChoice, RemoteConfig, StorageType};

/// The server the sign-in page opens on when the build was given one.
pub fn default_server_url() -> Option<&'static str> {
    option_env!("TRESSE_SERVER_URL").filter(|url| !url.trim().is_empty())
}

/// Opens the connection form for the folder at `path`; `changed` runs after a
/// save, so the list can read the folder again.
pub fn open(
    path: PathBuf,
    changed: Rc<dyn Fn(&mut Window, &mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| ConnectForm::new(path, changed, window, cx));
    let saving = form.clone();
    Form::new("Connection", form)
        .commit("Save", move |window, cx| {
            let saved = saving.update(cx, |form, cx| form.submit(cx));
            if saved {
                let changed = saving.read(cx).changed.clone();
                changed(window, cx);
                Notice::new("Connection saved. Sync reads it from the folder.")
                    .tone(Tone::Success)
                    .push(window, cx);
            }
            saved
        })
        .width(rems(46.))
        .open(window, cx);
}

/// How an imported string becomes a remote: a new repository gets a key
/// generated here, an existing one keeps the key of the storage on disk.
pub fn import(text: &str, previous: Option<&RemoteConfig>) -> Result<RemoteConfig, String> {
    let incoming = RemoteConfig::from_share_string(text)?;
    let choice = tresse_client::choose_key(
        &incoming,
        tresse_client::marks_new_repository(text),
        previous,
    )
    .map_err(|error| error.to_string())?;
    let mut remote = incoming;
    match choice {
        KeyChoice::Carried => {}
        KeyChoice::Generate => {
            remote.encryption_key = tresse_client::generate_encryption_key()?;
        }
        KeyChoice::Keep(key) => remote.encryption_key = key,
    }
    Ok(remote)
}

fn read_remote(path: &Path) -> Result<Option<RemoteConfig>, String> {
    if !path.is_dir() {
        return Err("The folder is not there.".to_string());
    }
    let repo = tresse_client::Repo::open_fs(path).map_err(|e| e.to_string())?;
    tresse_client::config::read_remote(repo.vault().as_ref())
}

/// Where a remote keeps the folder, in a few words: the server's host, or
/// the bucket and its prefix.
pub fn describe(remote: &RemoteConfig) -> String {
    fn host(url: &str) -> &str {
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        rest.split('/').next().unwrap_or(rest)
    }
    match remote.storage_type {
        StorageType::Http => host(&remote.url).to_string(),
        StorageType::S3 if remote.repo_id.is_empty() => {
            format!("{} on {}", remote.s3_bucket, host(&remote.s3_endpoint))
        }
        StorageType::S3 => format!(
            "{}/{} on {}",
            remote.s3_bucket,
            remote.repo_id,
            host(&remote.s3_endpoint)
        ),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Server,
    Custom,
}

impl Tab {
    fn key(self) -> &'static str {
        match self {
            Tab::Server => "server",
            Tab::Custom => "custom",
        }
    }

    fn from_key(key: &str) -> Self {
        if key == "custom" {
            Tab::Custom
        } else {
            Tab::Server
        }
    }
}

struct ConnectForm {
    path: PathBuf,
    tab: Tab,
    /// The server the sign-in page opens on.
    server_url: Entity<TextFieldState>,
    config_string: Entity<TextFieldState>,
    remote: RemoteFields,
    /// The remote as it is saved, to tell an unsaved change from it.
    saved: Option<RemoteConfig>,
    error: Option<SharedString>,
    changed: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl ConnectForm {
    fn new(
        path: PathBuf,
        changed: Rc<dyn Fn(&mut Window, &mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let loaded = read_remote(&path);
        let error = loaded.as_ref().err().cloned().map(Into::into);
        let saved = loaded.unwrap_or_default();
        let mut field = |purpose: Purpose, value: &str, placeholder: &'static str| {
            let value = value.to_string();
            cx.new(|cx| {
                let mut state = TextFieldState::new(purpose, window, cx);
                state.set_placeholder(placeholder, window, cx);
                state.set_value(value, window, cx);
                state
            })
        };
        // A folder on a server opens on the server it is on.
        let server = saved
            .as_ref()
            .filter(|remote| remote.storage_type == StorageType::Http)
            .map(|remote| remote.url.clone())
            .or_else(|| default_server_url().map(str::to_string))
            .unwrap_or_default();
        let server_url = field(Purpose::Address, &server, "https://…");
        let config_string = field(Purpose::Address, "", "tresse://…");
        ConnectForm {
            path,
            tab: Tab::Server,
            server_url,
            config_string,
            remote: RemoteFields::new(saved.as_ref(), window, cx),
            saved,
            error,
            changed,
        }
    }

    /// A command of the form that works on the form itself.
    fn command(
        &self,
        id: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
        run: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Command {
        let this = cx.entity().downgrade();
        Command::new(id, label).on_trigger(move |window, cx| {
            this.update(cx, |this, cx| run(this, window, cx)).ok();
        })
    }

    /// Starts the browser round trip to the server named in the field.
    fn open_server_page(&mut self, cx: &mut Context<Self>) {
        let server = self.server_url.read(cx).value(cx).trim().to_string();
        if server.is_empty() {
            self.error = Some("Enter the server's address.".into());
            cx.notify();
            return;
        }
        let page = tresse_client::connect_url(&server, None);
        gpui_android_glue::open_url(&page, cx);
        self.error = None;
        cx.notify();
    }

    /// Fills the fields from the pasted string.
    fn apply_string(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.config_string.read(cx).value(cx);
        let previous = self.saved.clone();
        match import(&text, previous.as_ref()) {
            Ok(remote) => {
                self.remote.fill(&remote, window, cx);
                self.config_string
                    .update(cx, |field, cx| field.set_value("", window, cx));
                self.error = None;
            }
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    /// Puts the fields on the clipboard as one string, for the next device.
    fn copy_string(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let remote = match self.remote.read(cx) {
            Ok(remote) => remote,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        match remote.validate() {
            Ok(()) => {
                cx.write_to_clipboard(ClipboardItem::new_string(remote.to_share_string()));
                gpui_android_glue::clipboard::push(cx);
                self.error = None;
                Notice::new(
                    "Connection string copied. It holds the encryption key: keep it like a password.",
                )
                .push(window, cx);
            }
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    fn generate_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.remote.has_key(cx) {
            return;
        }
        match tresse_client::generate_encryption_key() {
            Ok(key) => {
                self.remote.set_key(&key, window, cx);
                self.error = None;
            }
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    /// Records the remote. Answers whether it did.
    fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        let remote = match self.remote.read(cx) {
            Ok(remote) => remote,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return false;
            }
        };
        let remote = if RemoteFields::is_blank(&remote) {
            None
        } else if let Err(error) = remote.validate() {
            self.error = Some(error.into());
            cx.notify();
            return false;
        } else {
            Some(remote)
        };
        let written = tresse_client::Repo::open_fs(&self.path)
            .map_err(|error| error.to_string())
            .and_then(|repo| tresse_client::config::write_remote(&repo, remote.as_ref()));
        match written {
            Ok(()) => {
                self.saved = remote;
                self.error = None;
                cx.notify();
                true
            }
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                false
            }
        }
    }

    /// Where the folder syncs now, and whether the form holds a change of it
    /// that Save has yet to make.
    fn render_status(&self, cx: &mut Context<Self>) -> Note {
        let current = self
            .remote
            .read(cx)
            .ok()
            .filter(|remote| !RemoteFields::is_blank(remote));
        match (&self.saved, current) {
            (Some(saved), Some(current)) if *saved == current => {
                Note::new(format!("Syncing with {}.", describe(saved)))
            }
            (_, Some(current)) => {
                Note::new(format!("Save to sync with {}.", describe(&current))).tone(Tone::Info)
            }
            (Some(_), None) => {
                Note::new("Save to stop syncing. The folder and its history stay where they are.")
                    .tone(Tone::Info)
            }
            (None, None) => Note::new("Not connected: the folder is only on this device."),
        }
    }

    /// The short way: a server's page, a connection string, and the
    /// connection exported as one.
    fn render_server_tab(&self, cx: &mut Context<Self>) -> Fields {
        let gap = cx.scale().space_s;
        let open = self.command("connect-server", "Sign in", cx, |this, _, cx| {
            this.open_server_page(cx)
        });
        let fill = self.command("apply-string", "Use", cx, |this, window, cx| {
            this.apply_string(window, cx)
        });
        let export = self.command(
            "copy-string",
            "Export connection string",
            cx,
            |this, window, cx| this.copy_string(window, cx),
        );
        let has_storage = self
            .remote
            .read(cx)
            .is_ok_and(|remote| !RemoteFields::is_blank(&remote));
        Fields::new()
            .child(
                row()
                    .gap(gap)
                    .items_end()
                    .child(
                        gpui_kit::div()
                            .flex_1()
                            .min_w_0()
                            .child(TextField::new(&self.server_url).label("Server")),
                    )
                    .child(open.as_button()),
            )
            .child(Note::new(
                "Sign in opens the server in the browser. On a phone it brings you back to the \
                 app; on a desktop copy the connection string from the page and use it below.",
            ))
            .child(
                row()
                    .gap(gap)
                    .items_end()
                    .child(
                        gpui_kit::div()
                            .flex_1()
                            .min_w_0()
                            .child(TextField::new(&self.config_string).label("Connection string")),
                    )
                    .child(fill.as_button()),
            )
            .child(Note::new(
                "From another device or the server's page, if the browser did not bring you back.",
            ))
            .children(has_storage.then(|| {
                Setting::new("Another device")
                    .detail(
                        "Copies this connection as one string. It holds the encryption key: keep \
                         it like a password.",
                    )
                    .control(export.as_button())
            }))
    }

    /// Every field of the connection.
    fn render_custom_tab(&self, cx: &mut Context<Self>) -> Fields {
        let gap = cx.scale().space_s;
        let clear = self.command(
            "clear-connection",
            "Clear fields",
            cx,
            |this, window, cx| {
                this.remote.fill(&RemoteConfig::default(), window, cx);
                this.error = None;
                cx.notify();
            },
        );
        let generate = (!self.remote.has_key(cx)).then(|| {
            self.command(
                "generate-key",
                "Generate encryption key",
                cx,
                |this, window, cx| this.generate_key(window, cx),
            )
        });
        let this = cx.entity().downgrade();
        self.remote
            .render(move |kind, _, cx| {
                this.update(cx, |this, cx| {
                    this.remote.set_backend(kind);
                    cx.notify();
                })
                .ok();
            })
            .child(
                row()
                    .flex_wrap()
                    .gap(gap)
                    .children(generate.map(Command::as_button))
                    .child(clear.as_button()),
            )
    }
}

impl Render for ConnectForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let tabs = Segments::new("connection-tab", self.tab.key())
            .segment(Tab::Server.key(), "Server")
            .segment(Tab::Custom.key(), "Custom storage")
            .on_change(move |key, _, cx| {
                let tab = Tab::from_key(key);
                this.update(cx, |this, cx| {
                    this.tab = tab;
                    cx.notify();
                })
                .ok();
            });
        let body = match self.tab {
            Tab::Server => self.render_server_tab(cx),
            Tab::Custom => self.render_custom_tab(cx),
        };
        Fields::new()
            .child(self.render_status(cx))
            .child(tabs)
            .child(body)
            .children(
                self.error
                    .clone()
                    .map(|error| Note::new(error).tone(Tone::Danger)),
            )
    }
}

/// The fields of a remote, shared by the connection form.
struct RemoteFields {
    backend: StorageType,
    url: Entity<TextFieldState>,
    token: Entity<TextFieldState>,
    endpoint: Entity<TextFieldState>,
    region: Entity<TextFieldState>,
    bucket: Entity<TextFieldState>,
    access_key_id: Entity<TextFieldState>,
    secret_access_key: Entity<TextFieldState>,
    repo_id: Entity<TextFieldState>,
    encryption_key: Entity<TextFieldState>,
}

impl RemoteFields {
    /// The fields, holding `remote` when there is one to edit.
    fn new<T: 'static>(
        remote: Option<&RemoteConfig>,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Self {
        let mut field = |purpose: Purpose, value: Option<&str>, placeholder: &'static str| {
            let value = value.unwrap_or_default().to_string();
            cx.new(|cx| {
                let mut state = TextFieldState::new(purpose, window, cx);
                state.set_placeholder(placeholder, window, cx);
                state.set_value(value, window, cx);
                state
            })
        };
        let url = field(
            Purpose::Address,
            remote.map(|r| r.url.as_str()),
            "https://sync.example.com",
        );
        let token = field(Purpose::Password, remote.map(|r| r.token.as_str()), "");
        let endpoint = field(
            Purpose::Address,
            remote.map(|r| r.s3_endpoint.as_str()),
            "https://s3.us-east-1.amazonaws.com",
        );
        let region = field(
            Purpose::Plain,
            remote.map(|r| r.s3_region.as_str()),
            "us-east-1",
        );
        let bucket = field(
            Purpose::Plain,
            remote.map(|r| r.s3_bucket.as_str()),
            "my-bucket",
        );
        let access_key_id = field(
            Purpose::Login,
            remote.map(|r| r.s3_access_key_id.as_str()),
            "",
        );
        let secret_access_key = field(
            Purpose::Password,
            remote.map(|r| r.s3_secret_access_key.as_str()),
            "",
        );
        let repo_id = field(Purpose::Plain, remote.map(|r| r.repo_id.as_str()), "notes");
        let encryption_key = field(
            Purpose::Password,
            remote.map(|r| r.encryption_key.as_str()),
            "Required: 32-byte base64 key",
        );
        RemoteFields {
            backend: remote.map_or(StorageType::S3, |r| r.storage_type),
            url,
            token,
            endpoint,
            region,
            bucket,
            access_key_id,
            secret_access_key,
            repo_id,
            encryption_key,
        }
    }

    /// Every field, in the order they are laid out.
    fn fields(&self) -> [&Entity<TextFieldState>; 9] {
        [
            &self.url,
            &self.token,
            &self.endpoint,
            &self.region,
            &self.bucket,
            &self.access_key_id,
            &self.secret_access_key,
            &self.repo_id,
            &self.encryption_key,
        ]
    }

    fn set_backend(&mut self, backend: StorageType) {
        self.backend = backend;
    }

    /// What the fields currently say, trimmed. The secret is trimmed here
    /// only: stores hand it out without spaces, and one pasted with a
    /// trailing newline is the usual mistake.
    fn read(&self, cx: &App) -> Result<RemoteConfig, String> {
        let text = |field: &Entity<TextFieldState>| field.read(cx).value(cx).trim().to_string();
        Ok(RemoteConfig {
            storage_type: self.backend,
            url: text(&self.url),
            token: text(&self.token),
            s3_endpoint: text(&self.endpoint),
            s3_region: text(&self.region),
            s3_bucket: text(&self.bucket),
            s3_access_key_id: text(&self.access_key_id),
            s3_secret_access_key: text(&self.secret_access_key),
            repo_id: text(&self.repo_id),
            encryption_key: text(&self.encryption_key),
        })
    }

    /// Replaces what the fields hold, as pasting a connection string does.
    fn fill(&mut self, remote: &RemoteConfig, window: &mut Window, cx: &mut App) {
        self.backend = remote.storage_type;
        let values = [
            remote.url.clone(),
            remote.token.clone(),
            remote.s3_endpoint.clone(),
            remote.s3_region.clone(),
            remote.s3_bucket.clone(),
            remote.s3_access_key_id.clone(),
            remote.s3_secret_access_key.clone(),
            remote.repo_id.clone(),
            remote.encryption_key.clone(),
        ];
        for (field, value) in self.fields().into_iter().zip(values) {
            field.update(cx, |field, cx| field.set_value(value, window, cx));
        }
    }

    /// Whether every field is empty, which is how "no storage" is said.
    fn is_blank(remote: &RemoteConfig) -> bool {
        remote.url.is_empty()
            && remote.token.is_empty()
            && remote.repo_id.is_empty()
            && remote.encryption_key.is_empty()
            && remote.s3_endpoint.is_empty()
            && remote.s3_region.is_empty()
            && remote.s3_bucket.is_empty()
            && remote.s3_access_key_id.is_empty()
            && remote.s3_secret_access_key.is_empty()
    }

    /// Whether the key field holds anything: a key is only generated into an
    /// empty one.
    fn has_key(&self, cx: &App) -> bool {
        !self.encryption_key.read(cx).value(cx).trim().is_empty()
    }

    /// A new key, for a storage that has none yet. An existing key is never
    /// replaced: doing so would lock the folder out of its own history.
    fn set_key(&mut self, key: &str, window: &mut Window, cx: &mut App) {
        self.encryption_key
            .update(cx, |field, cx| field.set_value(key, window, cx));
    }

    /// The fields of the storage type chosen, under the choice of it.
    fn render(&self, on_backend: impl Fn(StorageType, &mut Window, &mut App) + 'static) -> Fields {
        let is_http = self.backend == StorageType::Http;
        let backend = Segments::new("remote-backend", if is_http { "http" } else { "s3" })
            .segment("http", "Tresse server")
            .segment("s3", "Own bucket")
            .on_change(move |key, window, cx| {
                let backend = if key.as_ref() == "http" {
                    StorageType::Http
                } else {
                    StorageType::S3
                };
                on_backend(backend, window, cx)
            });
        let fields = Fields::new().child(Labelled::new("Storage type", backend));
        let fields = if is_http {
            fields
                .child(TextField::new(&self.url).label("Server URL"))
                .child(TextField::new(&self.token).label("Access token"))
        } else {
            fields
                .child(TextField::new(&self.endpoint).label("Endpoint"))
                .child(TextField::new(&self.region).label("Region"))
                .child(TextField::new(&self.bucket).label("Bucket"))
                .child(TextField::new(&self.access_key_id).label("Access key ID"))
                .child(TextField::new(&self.secret_access_key).label("Secret access key"))
        };
        fields
            .child(TextField::new(&self.repo_id).label("Repository ID"))
            .child(TextField::new(&self.encryption_key).label("Encryption key"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_is_described_by_where_it_is() {
        let server = RemoteConfig {
            storage_type: StorageType::Http,
            url: "https://sync.example.com/".into(),
            ..Default::default()
        };
        assert_eq!(describe(&server), "sync.example.com");
        let bucket = RemoteConfig {
            storage_type: StorageType::S3,
            s3_endpoint: "https://s3.eu-central-1.amazonaws.com".into(),
            s3_bucket: "notes".into(),
            repo_id: "vault".into(),
            ..Default::default()
        };
        assert_eq!(
            describe(&bucket),
            "notes/vault on s3.eu-central-1.amazonaws.com"
        );
    }

    #[test]
    fn an_imported_string_for_a_new_repository_gets_a_generated_key() {
        let mut incoming = RemoteConfig {
            storage_type: StorageType::Http,
            url: "https://sync.example.com".into(),
            token: "token".into(),
            repo_id: "notes".into(),
            ..Default::default()
        };
        let mut text = incoming.to_share_string();
        text.push_str("&new=1");
        let imported = import(&text, None).unwrap();
        assert!(!imported.encryption_key.is_empty());
        assert_eq!(imported.url, incoming.url);
        incoming.encryption_key = imported.encryption_key;
    }

    #[test]
    fn an_existing_repository_without_a_key_keeps_the_one_on_disk() {
        let previous = RemoteConfig {
            storage_type: StorageType::Http,
            url: "https://sync.example.com".into(),
            token: "old".into(),
            repo_id: "notes".into(),
            encryption_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
            ..Default::default()
        };
        let mut incoming = previous.clone();
        incoming.token = "new".into();
        incoming.encryption_key = String::new();
        let string = incoming.to_share_string();
        let imported = import(&string, Some(&previous)).unwrap();
        assert_eq!(imported.encryption_key, previous.encryption_key);
        assert_eq!(imported.token, "new");
    }
}
