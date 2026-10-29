use std::fs;
use std::sync::{Mutex, MutexGuard};

use jikji_core::PrepareOptions;
use jikji_core::storage::{
    count_artifacts_with_ext, has_artifacts, indexed_roots, load_artifact, load_artifacts,
    root_statistics,
};
use jikji_index::{doctor, prepare};

static DATA_DIR_LOCK: Mutex<()> = Mutex::new(());

fn isolate_data_dir() -> (MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = DATA_DIR_LOCK.lock().expect("data dir lock");
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("JIKJI_DATA_DIR", data.path()) };
    (guard, data)
}

fn live_count(root: &std::path::Path, kind: &str) -> usize {
    load_artifacts(root, kind)
        .unwrap()
        .iter()
        .filter(|row| row.get("status").and_then(|v| v.as_str()) != Some("deleted"))
        .count()
}

#[test]
fn root_statistics_match_full_row_counts_and_exclude_deleted_files() {
    let (_lock, _data) = isolate_data_dir();
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("notes")).unwrap();
    fs::write(temp.path().join("keep.txt"), "kept body").unwrap();
    fs::write(temp.path().join("gone.txt"), "removed body").unwrap();
    fs::write(temp.path().join("notes/memo.md"), "# memo").unwrap();
    prepare(temp.path(), &PrepareOptions::default()).unwrap();

    fs::remove_file(temp.path().join("gone.txt")).unwrap();
    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    let tombstones = load_artifacts(temp.path(), "files")
        .unwrap()
        .iter()
        .filter(|row| row["status"] == "deleted")
        .count();
    assert_eq!(
        tombstones, 1,
        "the removed file must stay as a deleted tombstone"
    );

    let stats = root_statistics(temp.path()).unwrap();
    assert_eq!(stats.files, live_count(temp.path(), "files"));
    assert_eq!(stats.files, 2, "deleted files are not counted");
    assert_eq!(stats.folders, live_count(temp.path(), "folders"));
    assert_eq!(stats.documents, live_count(temp.path(), "documents"));
    assert_eq!(stats.chunks, live_count(temp.path(), "chunks"));

    let canonical = temp.path().canonicalize().unwrap();
    let listed = indexed_roots().unwrap();
    let entry = listed
        .iter()
        .find(|item| item.root == canonical)
        .expect("root listed");
    assert_eq!(entry.statistics, stats, "/api/roots and /api/status agree");
}

#[test]
fn light_lookups_agree_with_full_loads() {
    let (_lock, _data) = isolate_data_dir();
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("a.txt"), "alpha").unwrap();
    fs::write(temp.path().join("b.txt"), "beta").unwrap();

    assert!(
        !has_artifacts(temp.path(), "files").unwrap(),
        "unprepared root has no rows"
    );
    assert_eq!(load_artifact(temp.path(), "manifest").unwrap(), None);

    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    for kind in [
        "manifest",
        "files",
        "folders",
        "cards",
        "graph",
        "wiki_index",
    ] {
        assert_eq!(
            has_artifacts(temp.path(), kind).unwrap(),
            !load_artifacts(temp.path(), kind).unwrap().is_empty(),
            "has_artifacts({kind})"
        );
    }
    assert_eq!(
        load_artifact(temp.path(), "files").unwrap(),
        load_artifacts(temp.path(), "files")
            .unwrap()
            .into_iter()
            .next(),
        "load_artifact returns the first row in order"
    );
    let txt = load_artifacts(temp.path(), "files")
        .unwrap()
        .iter()
        .filter(|row| row["ext"] == ".txt")
        .count();
    assert_eq!(
        count_artifacts_with_ext(temp.path(), "files", &[".txt"]).unwrap(),
        txt
    );
    assert_eq!(
        count_artifacts_with_ext(temp.path(), "files", &[".png"]).unwrap(),
        0
    );
    assert_eq!(
        count_artifacts_with_ext(temp.path(), "files", &[]).unwrap(),
        0
    );

    let report = doctor(temp.path()).unwrap();
    assert!(
        report.prepared,
        "doctor still reports a prepared root: {:?}",
        report.errors
    );
}
