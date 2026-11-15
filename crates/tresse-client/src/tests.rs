use std::path::Path;
use std::process::Command;

use rdx_rs::{RdxElement, RdxValue, Stamp};
use rdx_sync::MemRemoteKv;

use crate::storage::dir::DirRemote;
use crate::{RemoteConfig, Repo, StorageType, TresseConfig, config};

fn remote() -> RemoteConfig {
    RemoteConfig {
        storage_type: StorageType::S3,
        repo_id: "ledger".into(),
        encryption_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
        s3_endpoint: "https://s3.example.com".into(),
        s3_bucket: "test".into(),
        s3_access_key_id: "test".into(),
        s3_secret_access_key: "test".into(),
        ..Default::default()
    }
}

fn setup(root: &Path) -> Repo {
    let repo = Repo::open_fs(root).unwrap();
    crate::init(&repo).unwrap();
    config::write_remote(&repo, Some(&remote())).unwrap();
    repo
}

fn record(key: &str, source: u64, time: u64) -> RdxElement {
    RdxElement::new(RdxValue::Eulerian(vec![RdxElement::with_stamp(
        RdxValue::Tuple(vec![RdxElement::new(RdxValue::Str(key.into()))]),
        Stamp::new(source, time),
    )]))
}

fn write(repo: &Repo, root: &Path, doc: &RdxElement) {
    crate::edit(repo, || {
        std::fs::write(root.join("ledger.rdx"), rdx_rs::encode(doc)).map_err(|e| e.to_string())
    })
    .unwrap()
}

fn read(root: &Path) -> RdxElement {
    rdx_rs::decode(&std::fs::read(root.join("ledger.rdx")).unwrap())
        .unwrap()
        .0
}

#[test]
fn worktree_edits_do_not_create_versions_or_cas_objects() {
    use rdx_sync::{ChunkClass, ObjectStore};

    let dir = tempfile::tempdir().unwrap();
    let repo = setup(dir.path());
    write(&repo, dir.path(), &record("baseline", 1, 64));
    crate::commit(&repo).unwrap();
    let local = crate::cas::open_local_store(&repo).unwrap();
    let meta = local.list_objects(ChunkClass::Meta).unwrap();
    let content = local.list_objects(ChunkClass::Content).unwrap();
    for time in [128, 192, 256] {
        write(&repo, dir.path(), &record("baseline", 1, time));
    }
    crate::recover(&repo).unwrap();
    assert_eq!(read(dir.path()), record("baseline", 1, 256));
    assert_eq!(local.list_objects(ChunkClass::Meta).unwrap(), meta);
    assert_eq!(local.list_objects(ChunkClass::Content).unwrap(), content);
    assert_eq!(
        crate::cas::load_merged_meta(&local).unwrap().versions.len(),
        1
    );
    crate::commit(&repo).unwrap();
    assert_eq!(
        crate::cas::load_merged_meta(&local).unwrap().versions.len(),
        2
    );
}

#[test]
fn an_edit_restores_pending_remote_changes_before_reading_the_worktree() {
    use rdx_sync::{ChunkClass, ObjectStore};

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let repo_a = setup(a.path());
    let repo_b = setup(b.path());
    let mut transport = MemRemoteKv::new();
    write(&repo_a, a.path(), &record("remote", 1, 64));
    crate::sync_with(&repo_a, &mut transport).unwrap();
    crate::commands::sync::exchange(&repo_b, &mut transport, &[0; 32]).unwrap();
    crate::hydrate(&repo_b, &transport, &[0; 32]).unwrap();
    assert!(b.path().join(tresse_lib::operation::PATH).exists());
    let local = crate::cas::open_local_store(&repo_b).unwrap();
    let meta = local.list_objects(ChunkClass::Meta).unwrap();
    let content = local.list_objects(ChunkClass::Content).unwrap();
    let amended = rdx_rs::merge(&record("remote", 1, 64), &record("local", 2, 128));
    crate::edit(&repo_b, || {
        assert_eq!(read(b.path()), record("remote", 1, 64));
        std::fs::write(b.path().join("ledger.rdx"), rdx_rs::encode(&amended))
            .map_err(|e| e.to_string())
    })
    .unwrap();
    crate::recover(&repo_b).unwrap();
    assert_eq!(read(b.path()), amended);
    assert!(!b.path().join(tresse_lib::operation::PATH).exists());
    assert_eq!(local.list_objects(ChunkClass::Meta).unwrap(), meta);
    assert_eq!(local.list_objects(ChunkClass::Content).unwrap(), content);
}

#[test]
fn remotes_are_read_from_disk_after_another_client_changes_them() {
    let dir = tempfile::tempdir().unwrap();
    let repo = setup(dir.path());
    let mut changed = remote();
    changed.repo_id = "other".into();
    changed.write_origin(repo.vault().as_ref()).unwrap();
    assert_eq!(
        config::read_remote(repo.vault().as_ref()).unwrap(),
        Some(changed)
    );
}

#[test]
fn edits_are_refused_while_another_client_holds_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    let repo = setup(dir.path());
    let _lock = crate::operation::RepoLock::acquire(&repo).unwrap();
    let mut called = false;
    let result = crate::edit(&repo, || {
        called = true;
        Ok(())
    });
    assert!(result.unwrap_err().contains("another tresse operation"));
    assert!(!called);
}

#[test]
fn equal_stat_metadata_does_not_hide_a_native_rdx_edit() {
    let dir = tempfile::tempdir().unwrap();
    let repo = setup(dir.path());
    write(
        &repo,
        dir.path(),
        &RdxElement::with_stamp(RdxValue::Str("a".into()), Stamp::new(1, 64)),
    );
    crate::commit(&repo).unwrap();
    let path = dir.path().join("ledger.rdx");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    crate::edit(&repo, || {
        std::fs::write(
            &path,
            rdx_rs::encode(&RdxElement::with_stamp(
                RdxValue::Str("b".into()),
                Stamp::new(1, 128),
            )),
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        Ok(())
    })
    .unwrap();
    crate::commit(&repo).unwrap();
    let local = crate::cas::open_local_store(&repo).unwrap();
    assert_eq!(
        crate::cas::load_merged_meta(&local).unwrap().versions.len(),
        2
    );
}

#[test]
fn an_encrypted_directory_remote_exchanges_native_documents() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let mut transport = DirRemote::open(remote_dir.path());
    let repo_a = setup(a.path());
    let repo_b = setup(b.path());
    write(&repo_a, a.path(), &record("first", 1, 64));
    crate::sync_with(&repo_a, &mut transport).unwrap();
    crate::sync_with(&repo_b, &mut transport).unwrap();
    assert_eq!(read(a.path()), read(b.path()));
    write(
        &repo_b,
        b.path(),
        &rdx_rs::merge(&read(b.path()), &record("second", 2, 128)),
    );
    crate::sync_with(&repo_b, &mut transport).unwrap();
    crate::sync_with(&repo_a, &mut transport).unwrap();
    assert_eq!(read(a.path()), read(b.path()));
}

#[test]
fn an_idle_exchange_sends_no_objects() {
    let dir = tempfile::tempdir().unwrap();
    let repo = setup(dir.path());
    write(&repo, dir.path(), &record("first", 1, 64));
    let mut transport = MemRemoteKv::new();
    crate::sync_with(&repo, &mut transport).unwrap();
    assert_eq!(crate::sync_with(&repo, &mut transport).unwrap(), (0, 0));
}

#[test]
fn tracked_ignore_rules_are_respected_without_client_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let repo = setup(dir.path());
    std::fs::write(
        dir.path().join("tresse.toml"),
        "[ignore]\npatterns = [\"private\"]\n[languages.rdx]\nstrategy = \"native\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("private"), "local").unwrap();
    std::fs::write(dir.path().join("visible"), "tracked").unwrap();
    crate::commit(&repo).unwrap();
    let local = crate::cas::open_local_store(&repo).unwrap();
    let meta = crate::cas::load_merged_meta(&local).unwrap();
    assert!(!meta.tree.contains_key("private"));
    assert!(meta.tree.contains_key("visible"));
}

#[test]
#[ignore = "requires TRESSE_CLI_BIN pointing to the matching upstream Tresse CLI"]
fn upstream_cli_and_embedded_client_share_config_history_locks_and_sync() {
    let cli = std::env::var_os("TRESSE_CLI_BIN").expect("set TRESSE_CLI_BIN");
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let run = |root: &Path, args: &[&str]| {
        let output = Command::new(&cli)
            .current_dir(root)
            .args(args)
            .env("TRESSE_TEST_REMOTE_DIR", remote_dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let repo_a = setup(a.path());
    write(&repo_a, a.path(), &record("first", 1, 64));
    let exported = run(a.path(), &["config", "export"]);
    assert_eq!(
        RemoteConfig::from_share_string(exported.trim()).unwrap(),
        remote()
    );
    run(a.path(), &["config", "validate"]);
    let mut transport = DirRemote::open(remote_dir.path());
    crate::sync_with(&repo_a, &mut transport).unwrap();
    run(b.path(), &["init"]);
    run(b.path(), &["config", "import", &exported]);
    let repo_b = Repo::open_fs(b.path()).unwrap();
    assert_eq!(
        TresseConfig::load(repo_b.vault().as_ref())
            .unwrap()
            .origin(),
        Some(&remote())
    );
    run(b.path(), &["sync"]);
    // The embedded client resumes the CLI's pending restore journal.
    crate::sync_with(&repo_b, &mut transport).unwrap();
    assert_eq!(read(a.path()), read(b.path()));
    let first_version =
        crate::cas::load_merged_meta(&crate::cas::open_local_store(&repo_b).unwrap())
            .unwrap()
            .versions;
    let latest = first_version
        .iter()
        .max_by_key(|stamp| stamp.source)
        .unwrap();
    let label = tresse_lib::VersionLabel::from_source(latest.source).format();
    let amended = rdx_rs::merge(&read(b.path()), &record("second", 2, 128));
    std::fs::write(b.path().join("ledger.rdx"), rdx_rs::encode(&amended)).unwrap();
    run(b.path(), &["commit"]);
    run(b.path(), &["sync"]);
    crate::sync_with(&repo_a, &mut transport).unwrap();
    assert_eq!(read(a.path()), amended);
    assert!(run(a.path(), &["log", "--changes"]).contains("ledger.rdx"));
    run(b.path(), &["restore", &label]);
    assert_eq!(read(b.path()), record("first", 1, 64));
    let _lock = crate::operation::RepoLock::acquire(&repo_a).unwrap();
    let locked = Command::new(&cli)
        .current_dir(a.path())
        .arg("commit")
        .output()
        .unwrap();
    assert!(!locked.status.success());
    assert!(String::from_utf8_lossy(&locked.stderr).contains("another tresse operation"));
}
