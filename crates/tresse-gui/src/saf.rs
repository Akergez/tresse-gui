//! The phone's vault: a folder picked through the Storage Access Framework.
//!
//! Android gives an application no path to write into: it gives a tree URI
//! and a grant. Everything the vault needs is a call into `SafBridge`, the
//! Java half in `android/app/src/main/java/app/akergez/tressegui`, which
//! speaks `DocumentsContract` and hands back JSON and bytes.
//!
//! The calls block on the provider, so they belong on a background thread;
//! running out of the grant is reported as "choose the folder again".

use std::fs::File;
use std::io;
use std::os::fd::FromRawFd;

use gpui_mobile::android::jni as bridge;
use jni::objects::{JClass, JObject, JValue};
use tresse_client::{Entry, Meta, Vault, VaultLock, Written};

const BRIDGE: &str = "app.akergez.tressegui.SafBridge";
const PICKER: &str = "app.akergez.tressegui.TreePicker";

/// A picked folder, reached through its grant.
pub struct SafVault {
    tree: String,
}

impl SafVault {
    /// A vault over a tree URI; refuses one whose grant is gone.
    pub fn open(tree: impl Into<String>) -> Result<Self, String> {
        let tree = tree.into();
        if !has_grant(&tree)? {
            return Err("The app may no longer open this folder. Choose it again.".to_string());
        }
        Ok(Self { tree })
    }
}

/// Whether the grant on the tree is still held, for reading and for writing.
pub fn has_grant(tree: &str) -> Result<bool, String> {
    with_class(BRIDGE, |env, activity, class| {
        let tree = env.new_string(tree)?;
        let value = env.call_static_method(
            class,
            jni::jni_str!("hasGrant"),
            jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)Z"),
            &[JValue::Object(activity), JValue::Object(tree.as_ref())],
        )?;
        value.z()
    })
}

/// Asks the person for a folder. Blocking: call from a background task.
/// `Ok(None)` is the person cancelling.
pub fn pick_folder() -> Result<Option<(String, String)>, String> {
    let result = with_class(PICKER, |env, activity, class| {
        let value = env.call_static_method(
            class,
            jni::jni_str!("pick"),
            jni::jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"),
            &[JValue::Object(activity)],
        )?;
        let object = value.l()?;
        if object.is_null() {
            return Ok(None);
        }
        Ok(Some(bridge::get_string(env, &object)))
    })?;
    let Some(json) = result else {
        return Ok(None);
    };
    #[derive(serde::Deserialize)]
    struct Picked {
        uri: String,
        name: String,
    }
    let picked: Picked = serde_json::from_str(&json).map_err(|error| error.to_string())?;
    Ok(Some((picked.uri, picked.name)))
}

impl SafVault {
    fn list_json(&self, dir: &str) -> Result<String, String> {
        let tree = &self.tree;
        with_class(BRIDGE, |env, activity, class| {
            let tree = env.new_string(tree)?;
            let dir = env.new_string(dir)?;
            let value = env.call_static_method(
                class,
                jni::jni_str!("list"),
                jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"),
                &[
                    JValue::Object(activity),
                    JValue::Object(tree.as_ref()),
                    JValue::Object(dir.as_ref()),
                ],
            )?;
            let object = value.l()?;
            Ok(if object.is_null() {
                "[]".to_string()
            } else {
                bridge::get_string(env, &object)
            })
        })
    }

    fn metadata_json(&self, path: &str) -> Result<Option<String>, String> {
        let tree = &self.tree;
        with_class(BRIDGE, |env, activity, class| {
            let tree = env.new_string(tree)?;
            let path = env.new_string(path)?;
            let value = env.call_static_method(
                class,
                jni::jni_str!("metadata"),
                jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"),
                &[
                    JValue::Object(activity),
                    JValue::Object(tree.as_ref()),
                    JValue::Object(path.as_ref()),
                ],
            )?;
            let object = value.l()?;
            Ok(if object.is_null() {
                None
            } else {
                Some(bridge::get_string(env, &object))
            })
        })
    }

    fn read_bytes(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let tree = &self.tree;
        with_class(BRIDGE, |env, activity, class| {
            let tree = env.new_string(tree)?;
            let path = env.new_string(path)?;
            let value = env.call_static_method(
                class,
                jni::jni_str!("read"),
                jni::jni_sig!(
                    "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;JI)[B"
                ),
                &[
                    JValue::Object(activity),
                    JValue::Object(tree.as_ref()),
                    JValue::Object(path.as_ref()),
                    JValue::Long(offset as i64),
                    JValue::Int(len.min(i32::MAX as usize) as i32),
                ],
            )?;
            let object = value.l()?;
            if object.is_null() {
                return Ok(Vec::new());
            }
            let array = unsafe { jni::objects::JByteArray::from_raw(env, object.as_raw()) };
            env.convert_byte_array(&array)
        })
    }

    fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let tree = &self.tree;
        with_class(BRIDGE, |env, activity, class| {
            let tree = env.new_string(tree)?;
            let path = env.new_string(path)?;
            let payload = env.byte_array_from_slice(bytes)?;
            env.call_static_method(
                class,
                jni::jni_str!("write"),
                jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;[B)V"),
                &[
                    JValue::Object(activity),
                    JValue::Object(tree.as_ref()),
                    JValue::Object(path.as_ref()),
                    JValue::Object(payload.as_ref()),
                ],
            )?;
            Ok(())
        })
    }

    fn mkdir(&self, path: &str) -> Result<(), String> {
        let tree = &self.tree;
        with_class(BRIDGE, |env, activity, class| {
            let tree = env.new_string(tree)?;
            let path = env.new_string(path)?;
            env.call_static_method(
                class,
                jni::jni_str!("mkdir"),
                jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V"),
                &[
                    JValue::Object(activity),
                    JValue::Object(tree.as_ref()),
                    JValue::Object(path.as_ref()),
                ],
            )?;
            Ok(())
        })
    }

    fn remove(&self, path: &str, recursive: bool) -> Result<(), String> {
        let tree = &self.tree;
        with_class(BRIDGE, |env, activity, class| {
            let tree = env.new_string(tree)?;
            let path = env.new_string(path)?;
            env.call_static_method(
                class,
                jni::jni_str!("remove"),
                jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Z)V"),
                &[
                    JValue::Object(activity),
                    JValue::Object(tree.as_ref()),
                    JValue::Object(path.as_ref()),
                    JValue::Bool(recursive as jni::sys::jboolean),
                ],
            )?;
            Ok(())
        })
    }

    fn open_lock(&self) -> Result<i32, String> {
        let tree = &self.tree;
        with_class(BRIDGE, |env, activity, class| {
            let tree = env.new_string(tree)?;
            let value = env.call_static_method(
                class,
                jni::jni_str!("openLock"),
                jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)I"),
                &[JValue::Object(activity), JValue::Object(tree.as_ref())],
            )?;
            value.i()
        })
    }
}

#[derive(serde::Deserialize)]
struct RawEntry {
    name: String,
    dir: bool,
    len: u64,
    mtime: u64,
}

#[derive(serde::Deserialize)]
struct RawMeta {
    dir: bool,
    len: u64,
    mtime: u64,
}

fn meta_of(raw: RawMeta) -> Meta {
    Meta {
        is_dir: raw.dir,
        len: raw.len,
        mtime_ms: raw.mtime,
    }
}

impl Vault for SafVault {
    fn list(&self, dir: &str) -> io::Result<Vec<Entry>> {
        let json = self.list_json(dir).map_err(io::Error::other)?;
        let rows: Vec<RawEntry> =
            serde_json::from_str(&json).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| Entry {
                name: row.name,
                is_dir: row.dir,
                len: row.len,
                mtime_ms: row.mtime,
            })
            .collect())
    }

    fn metadata(&self, path: &str) -> io::Result<Option<Meta>> {
        let json = self.metadata_json(path).map_err(io::Error::other)?;
        let Some(json) = json else {
            return Ok(None);
        };
        let raw: RawMeta =
            serde_json::from_str(&json).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Some(meta_of(raw)))
    }

    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let meta = self.metadata(path)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{path} is not there"))
        })?;
        self.read_range(path, 0, meta.len as usize)
    }

    fn read_range(&self, path: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.read_bytes(path, offset, len).map_err(io::Error::other)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> io::Result<Written> {
        self.write_bytes(path, bytes).map_err(io::Error::other)?;
        let meta = self
            .metadata(path)?
            .ok_or_else(|| io::Error::other(format!("{path} vanished after it was written")))?;
        Ok(Written {
            size: meta.len,
            mtime_ms: meta.mtime_ms,
        })
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        self.remove(path, false).map_err(io::Error::other)
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        // An empty directory is what the prune asks for; a provider that
        // removes its children anyway was asked to remove the directory.
        self.remove(path, false).map_err(io::Error::other)
    }

    fn remove_dir_all(&self, path: &str) -> io::Result<()> {
        self.remove(path, true).map_err(io::Error::other)
    }

    fn mkdir_all(&self, dir: &str) -> io::Result<()> {
        if dir.is_empty() {
            return Ok(());
        }
        self.mkdir(dir).map_err(io::Error::other)
    }

    fn lock(&self) -> io::Result<Box<dyn VaultLock>> {
        let fd = self.open_lock().map_err(io::Error::other)?;
        // SAF handed the descriptor over detached: closing it is ours.
        let file = unsafe { File::from_raw_fd(fd) };
        match try_flock(&file) {
            Ok(()) => Ok(Box::new(SafLock { _file: file })),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another process holds the repository lock",
            )),
            Err(error) => Err(error),
        }
    }
}

struct SafLock {
    _file: File,
}

impl VaultLock for SafLock {}

fn try_flock(file: &File) -> io::Result<()> {
    use std::io::Error;
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: the borrowed File keeps its descriptor open for this call;
        // flock neither retains pointers nor takes ownership of the fd.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(());
        }
        let error = Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return Err(error),
            _ => return Err(error),
        }
    }
}

fn with_class<T, F>(class_name: &str, call: F) -> Result<T, String>
where
    F: for<'a> FnOnce(&mut jni::Env<'a>, &JObject<'a>, &JClass<'a>) -> jni::errors::Result<T>,
{
    bridge::with_env(|env| {
        let activity = bridge::activity(env)?;
        let class = bridge::find_app_class(env, class_name)?;
        call(env, &activity, &class).map_err(|error| {
            env.exception_clear();
            error.to_string()
        })
    })
}
