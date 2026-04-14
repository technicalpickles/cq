use tempfile::TempDir;

fn cache_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn creates_tables_on_first_open() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path()).unwrap();

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
    let conn = cq::cache::open(dir.path()).unwrap();
    drop(conn);

    // Second open should succeed without rebuilding
    let conn = cq::cache::open(dir.path()).unwrap();
    let version: i32 = conn
        .query_row("SELECT version FROM cache_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, cq::cache::SCHEMA_VERSION);
}

#[test]
fn version_mismatch_triggers_rebuild() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path()).unwrap();

    // Tamper with version
    conn.execute("UPDATE cache_meta SET version = 0", []).unwrap();

    // Insert a row that should disappear after rebuild
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size) VALUES ('ghost.jsonl', 0, 0)",
        [],
    ).unwrap();
    drop(conn);

    let conn = cq::cache::open(dir.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "rebuild should clear all data");
}
