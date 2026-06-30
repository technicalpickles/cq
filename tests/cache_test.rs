use std::path::PathBuf;
use tempfile::TempDir;

fn cache_dir() -> TempDir {
    TempDir::new().unwrap()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Create a fake projects dir with one project containing fixture files.
fn setup_projects(fixtures: &[&str]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();
    for fixture in fixtures {
        let src = fixture_path(fixture);
        let dest = project_dir.join(fixture);
        std::fs::copy(&src, &dest).unwrap();
    }
    tmp
}

#[test]
fn index_new_files() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    let result = cq::indexer::sync_sources(
        &conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.stats.added, 1);
    assert_eq!(result.stats.removed, 0);
    assert_eq!(result.stats.changed, 0);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM raw_records", [], |r| r.get(0))
        .unwrap();
    assert!(count > 0, "raw_records should have rows after indexing");

    let reg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(reg_count, 1);
}

#[test]
fn no_changes_is_noop() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    cq::indexer::sync_sources(
        &conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    let result = cq::indexer::sync_sources(
        &conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.stats.added, 0);
    assert_eq!(result.stats.removed, 0);
    assert_eq!(result.stats.changed, 0);
}

#[test]
fn detects_deleted_files() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl", "error_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    cq::indexer::sync_sources(
        &conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();

    let project_dir = projects.path().join("-Users-test-myproject");
    std::fs::remove_file(project_dir.join("error_session.jsonl")).unwrap();

    let result = cq::indexer::sync_sources(
        &conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.stats.removed, 1);

    let reg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(reg_count, 1, "only simple_session.jsonl should remain");
}

#[test]
fn detects_changed_files() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    cq::indexer::sync_sources(
        &conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();

    let project_dir = projects.path().join("-Users-test-myproject");
    let file_path = project_dir.join("simple_session.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file_path)
        .unwrap();
    use std::io::Write;
    writeln!(f, "{{}}").unwrap();

    let result = cq::indexer::sync_sources(
        &conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.stats.changed, 1);
}

#[test]
fn creates_tables_on_first_open() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    // Verify tables exist
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_name IN ('cache_meta', 'file_registry', 'raw_records')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
}

#[test]
fn version_check_passes_on_current() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();
    drop(conn);

    // Second open should succeed without rebuilding
    let conn = cq::cache::open(dir.path(), false).unwrap();
    let version: i32 = conn
        .query_row("SELECT version FROM cache_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, cq::cache::SCHEMA_VERSION);
}

#[test]
fn cache_meta_has_last_sync_at() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    let last_sync: i64 = conn
        .query_row("SELECT last_sync_at FROM cache_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(last_sync, 0);
}

#[test]
fn version_mismatch_triggers_rebuild() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    // Tamper with version
    conn.execute("UPDATE cache_meta SET version = 0", [])
        .unwrap();

    // Insert a row that should disappear after rebuild
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size) VALUES ('ghost.jsonl', 0, 0)",
        [],
    )
    .unwrap();
    drop(conn);

    let conn = cq::cache::open(dir.path(), false).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "rebuild should clear all data");
}

#[test]
fn auto_sync_skips_when_nothing_changed() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    let sources = vec![("main".to_string(), projects.path().to_path_buf())];
    let result = cq::indexer::sync_sources(
        &conn,
        &sources,
        cq::db::SyncMode::Auto,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.stats.added, 1);
    assert!(!result.skipped);

    let result = cq::indexer::sync_sources(
        &conn,
        &sources,
        cq::db::SyncMode::Auto,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.stats.added, 0);
    assert_eq!(result.stats.changed, 0);
    assert_eq!(result.stats.removed, 0);
}

#[test]
fn force_sync_always_scans() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    let sources = vec![("main".to_string(), projects.path().to_path_buf())];
    cq::indexer::sync_sources(
        &conn,
        &sources,
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();

    let result = cq::indexer::sync_sources(
        &conn,
        &sources,
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert!(!result.skipped);
}

#[test]
fn skip_sync_returns_immediately() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    let sources = vec![("main".to_string(), projects.path().to_path_buf())];
    let result = cq::indexer::sync_sources(
        &conn,
        &sources,
        cq::db::SyncMode::Skip,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
    assert!(result.skipped);
    assert_eq!(result.stats.added, 0);
}
