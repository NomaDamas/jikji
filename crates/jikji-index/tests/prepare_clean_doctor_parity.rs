use std::fs;
use std::sync::{Mutex, MutexGuard};

use jikji_core::PrepareOptions;
use jikji_core::storage::{clear_artifact, load_artifacts, open_database, register_root};
use jikji_index::{CleanOptions, clean, doctor, prepare, reindex_search};
use jikji_search::{IndexStatus, search_index_status};

static DATA_DIR_LOCK: Mutex<()> = Mutex::new(());

fn isolate_data_dir() -> (MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = DATA_DIR_LOCK.lock().expect("data dir lock");
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("JIKJI_DATA_DIR", data.path()) };
    (guard, data)
}

#[test]
fn library_prepare_persists_documents_and_clean_removes_central_root() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    fs::write(temp.path().join("document.txt"), "parser body marker").unwrap();

    let result = prepare(temp.path(), &PrepareOptions::default()).unwrap();
    assert_eq!(result.files, 1);
    assert!(!temp.path().join(".jikji").exists());
    let files = load_artifacts(temp.path(), "files").unwrap();
    assert_eq!(files[0]["path"], "document.txt");
    assert!(doctor(temp.path()).unwrap().ok);

    let cleaned = clean(
        temp.path(),
        CleanOptions {
            dry_run: false,
            force: false,
        },
    )
    .unwrap();
    assert!(cleaned.ok);
    let connection = open_database().unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    for table in [
        "artifacts",
        "search_meta",
        "search_docs",
        "search_terms",
        "search_filename_keys",
        "search_idf",
        "search_field_terms",
        "search_field_lengths",
        "search_field_idf",
        "search_field_avg",
    ] {
        let remaining: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 0, "table {table}");
    }
    assert!(temp.path().join("document.txt").is_file());
}

#[test]
fn prepare_rejects_new_overlapping_root_but_allows_exact_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    let child = temp.path().join("child");
    fs::create_dir(&child).unwrap();
    fs::write(child.join("document.txt"), "parent body").unwrap();

    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    let error = prepare(&child, &PrepareOptions::default()).expect_err("overlap guard");
    assert!(
        error
            .to_string()
            .contains("prepare root overlaps existing indexed root"),
        "{error}"
    );
}

#[test]
fn empty_registered_root_does_not_block_nested_prepare() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    let child = temp.path().join("child");
    fs::create_dir(&child).unwrap();
    fs::write(child.join("document.txt"), "child body").unwrap();

    register_root(temp.path()).unwrap();
    prepare(&child, &PrepareOptions::default()).unwrap();
    let connection = open_database().unwrap();
    let roots: i64 = connection
        .query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))
        .unwrap();
    assert_eq!(roots, 2);
}

#[test]
fn sibling_prefix_roots_are_not_treated_as_overlapping() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    let first = temp.path().join("root");
    let second = temp.path().join("root-copy");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("one.txt"), "one").unwrap();
    fs::write(second.join("two.txt"), "two").unwrap();

    prepare(&first, &PrepareOptions::default()).unwrap();
    prepare(&second, &PrepareOptions::default()).unwrap();
    let connection = open_database().unwrap();
    let roots: i64 = connection
        .query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))
        .unwrap();
    assert_eq!(roots, 2);
}

#[test]
fn search_only_rebuilds_search_docs_from_existing_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    fs::write(temp.path().join("resume.txt"), "정철현 이력서 body").unwrap();

    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    let connection = open_database().unwrap();
    let root_id: i64 = connection
        .query_row("SELECT id FROM roots LIMIT 1", [], |row| row.get(0))
        .unwrap();
    connection
        .execute("DELETE FROM search_docs WHERE root_id=?1", [root_id])
        .unwrap();
    let empty: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM search_docs WHERE root_id=?1",
            [root_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(empty, 0);

    let stats = reindex_search(temp.path()).unwrap();
    assert!(stats.rows >= 1, "search rows {}", stats.rows);
    let restored: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM search_docs WHERE root_id=?1",
            [root_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(restored >= 1, "restored search_docs {restored}");
    clear_artifact(temp.path(), "manifest").unwrap();
    let status = search_index_status(temp.path(), 86_400);
    assert_eq!(status.status, IndexStatus::Ready);
    assert!(!status.should_prepare);
}

#[test]
fn search_only_refuses_empty_files_artifacts_without_wiping_search_docs() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    fs::write(temp.path().join("resume.txt"), "정철현 이력서 body").unwrap();

    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    let connection = open_database().unwrap();
    let root_id: i64 = connection
        .query_row("SELECT id FROM roots LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let before: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM search_docs WHERE root_id=?1",
            [root_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(before >= 1, "prepared search_docs {before}");

    clear_artifact(temp.path(), "files").unwrap();
    let error = reindex_search(temp.path()).expect_err("empty files artifacts");
    assert!(
        error
            .to_string()
            .contains("search-only requires existing files artifacts"),
        "{error}"
    );
    let after: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM search_docs WHERE root_id=?1",
            [root_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
}

#[test]
fn search_index_status_ready_without_canonicalizing_missing_registered_root() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    fs::write(temp.path().join("resume.txt"), "정철현 이력서 body").unwrap();

    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    let connection = open_database().unwrap();
    let missing = temp.path().join("missing-GoogleDrive/마커");
    assert!(!missing.exists());
    connection
        .execute(
            "UPDATE roots SET canonical_root=?1",
            [missing.to_string_lossy().as_ref()],
        )
        .unwrap();
    clear_artifact(&missing, "manifest").unwrap();
    let status = search_index_status(&missing, 86_400);
    assert_eq!(status.status, IndexStatus::Ready);
    assert!(!status.should_prepare);
}

#[cfg(unix)]
#[test]
fn search_only_finds_registered_root_via_symlink_without_canonicalizing() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    fs::write(temp.path().join("resume.txt"), "정철현 이력서 body").unwrap();

    prepare(temp.path(), &PrepareOptions::default()).unwrap();
    let connection = open_database().unwrap();
    let root_id: i64 = connection
        .query_row("SELECT id FROM roots LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let target = temp.path().join("resolved-GoogleDrive/마커");
    let link = temp.path().join("Drive");
    assert!(!target.exists());
    std::os::unix::fs::symlink(&target, &link).unwrap();
    connection
        .execute(
            "UPDATE roots SET canonical_root=?1",
            [target.to_string_lossy().as_ref()],
        )
        .unwrap();
    clear_artifact(&link, "manifest").unwrap();
    let status = search_index_status(&link, 86_400);
    assert_eq!(status.status, IndexStatus::Ready);
    assert!(!status.should_prepare);

    let stats = reindex_search(&link).unwrap();
    assert!(stats.rows >= 1, "search rows {}", stats.rows);
    let same_id: i64 = connection
        .query_row(
            "SELECT id FROM roots WHERE canonical_root=?1",
            [target.to_string_lossy().as_ref()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(same_id, root_id);
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn clean_reindex_rebuilds_without_tombstones_or_source_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let (_lock, _data) = isolate_data_dir();
    let source = temp.path().join("keep.txt");
    fs::write(&source, "stable source").unwrap();
    let options = PrepareOptions {
        clean_reindex: true,
        ..Default::default()
    };
    prepare(temp.path(), &options).unwrap();
    fs::write(temp.path().join("transient.txt"), "transient").unwrap();
    prepare(temp.path(), &options).unwrap();
    let rows = load_artifacts(temp.path(), "files").unwrap();
    assert_eq!(
        rows.iter().filter(|row| row["path"] == "keep.txt").count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["path"] == "transient.txt")
            .count(),
        1
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "stable source");
}
