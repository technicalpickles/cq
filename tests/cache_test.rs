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

/// DuckDB's read_json(..., ignore_errors=true) tolerates a line it can't parse
/// (e.g. an unpaired UTF-16 surrogate escape, which lenient JSON parsers accept
/// but DuckDB's stricter parser rejects) by emitting that row with a NULL value
/// rather than dropping it or raising. raw_records.json is NOT NULL, so indexing
/// must filter those rows out instead of crashing on the constraint violation.
#[test]
fn skips_unparseable_record_instead_of_crashing() {
    let cache = cache_dir();
    let projects = setup_projects(&["unparseable_record.jsonl"]);
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

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM raw_records", [], |r| r.get(0))
        .unwrap();
    // The fixture has 3 lines; the middle one has an unpaired surrogate escape
    // and should be skipped, leaving the 2 well-formed records indexed.
    assert_eq!(
        count, 2,
        "the unparseable record should be skipped, not inserted as NULL"
    );

    let null_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM raw_records WHERE json IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        null_count, 0,
        "raw_records.json is NOT NULL; no NULL rows should ever be inserted"
    );
}

#[test]
fn creates_tables_on_first_open() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    // Verify tables exist
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_name IN ('cache_meta', 'file_registry', 'raw_records', 'messages_rows')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 4);
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

    let fts_sync: i64 = conn
        .query_row("SELECT fts_sync_at FROM cache_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts_sync, -1);

    let fts_slot: i32 = conn
        .query_row("SELECT fts_slot FROM cache_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts_slot, 0);
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

#[test]
fn fresh_cache_uses_current_storage_version() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    assert_eq!(
        cq::cache::storage_version(&conn).unwrap().as_deref(),
        Some(cq::cache::STORAGE_VERSION_TAG),
        "a new cache file should use DuckDB's latest storage format; if a DuckDB \
         upgrade changed the tag, update STORAGE_VERSION_TAG (existing caches \
         will be recreated once on first open)"
    );
}

#[test]
fn large_json_records_are_compressed() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    // ~11 KB per record, the size range that stayed Uncompressed under the
    // v0.10.2 storage format.
    conn.execute_batch(
        "INSERT INTO raw_records (source_file, json)
         SELECT 'big.jsonl', json_object('i', i, 'pad', repeat('lorem ipsum dolor ', 600))
         FROM range(2000) r(i);
         CHECKPOINT;",
    )
    .unwrap();

    let (uncompressed, compressed): (i64, i64) = conn
        .query_row(
            "SELECT count(*) FILTER (WHERE compression = 'Uncompressed'),
                    count(*) FILTER (WHERE compression <> 'Uncompressed')
             FROM pragma_storage_info('raw_records')
             WHERE column_name = 'json' AND segment_type <> 'VALIDITY'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        uncompressed, 0,
        "raw_records.json should not be stored Uncompressed"
    );
    assert!(
        compressed > 0,
        "expected at least one compressed json segment; none were inspected"
    );
}

/// Write a cache file in the given storage format whose cache_meta otherwise
/// passes the schema-version check, plus a marker table that only survives
/// if the file is NOT recreated.
fn write_cache_file(dir: &std::path::Path, storage: &str) {
    let config = duckdb::Config::default()
        .with("storage_compatibility_version", storage)
        .unwrap();
    let conn = duckdb::Connection::open_with_flags(dir.join("index.duckdb"), config).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE cache_meta (version INTEGER NOT NULL);
         INSERT INTO cache_meta VALUES ({});
         CREATE TABLE marker (x INTEGER);",
        cq::cache::SCHEMA_VERSION
    ))
    .unwrap();
}

fn marker_exists(conn: &duckdb::Connection) -> bool {
    conn.query_row(
        "SELECT count(*) > 0 FROM information_schema.tables WHERE table_name = 'marker'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn legacy_storage_file_is_recreated() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "v0.10.2");

    let conn = cq::cache::open(dir.path(), false).unwrap();
    assert!(
        !marker_exists(&conn),
        "legacy-format file should be recreated"
    );
    assert_eq!(
        cq::cache::storage_version(&conn).unwrap().as_deref(),
        Some(cq::cache::STORAGE_VERSION_TAG)
    );
}

#[test]
fn current_storage_file_is_kept() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "latest");

    let conn = cq::cache::open(dir.path(), false).unwrap();
    assert!(
        marker_exists(&conn),
        "a current-format file at the current schema version must not be recreated"
    );
}

#[test]
fn force_rebuild_recreates_file() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "latest");

    let conn = cq::cache::open(dir.path(), true).unwrap();
    assert!(
        !marker_exists(&conn),
        "--reindex should start from a fresh file"
    );
}

fn hold_index_lock(dir: &std::path::Path) -> std::fs::File {
    use fs2::FileExt;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("index.lock"))
        .unwrap();
    f.lock_exclusive().unwrap();
    f
}

#[test]
fn recreating_legacy_file_waits_on_index_lock() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "v0.10.2");
    let _held = hold_index_lock(dir.path());

    let err = cq::cache::open(dir.path(), false).unwrap_err();
    assert!(err.to_string().contains("locked"), "got: {err}");
}

#[test]
fn current_file_open_ignores_index_lock() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "latest");
    let _held = hold_index_lock(dir.path());

    let conn = cq::cache::open(dir.path(), false).unwrap();
    assert!(marker_exists(&conn));
}

#[test]
fn open_points_duckdb_spill_at_a_per_process_dir_in_the_cache() {
    let cache = cache_dir();
    let conn = cq::cache::open(cache.path(), false).unwrap();

    let temp_dir: String = conn
        .query_row("SELECT current_setting('temp_directory')", [], |r| r.get(0))
        .unwrap();
    let expected = cache
        .path()
        .join("tmp")
        .join(std::process::id().to_string());
    assert_eq!(temp_dir, expected.to_str().unwrap());
}

const MATERIALIZE_FIXTURES: &[&str] = &[
    "simple_session.jsonl",
    "multi_tool_session.jsonl",
    "multi_text_block_session.jsonl",
    "mixed_sidechain_session.jsonl",
    "mixed_types.jsonl",
    "prompt_provenance.jsonl",
    "first_user_message_session.jsonl",
    "context_session.jsonl",
    "subagent_other_cwd.jsonl",
];

fn sync_main(conn: &duckdb::Connection, cache: &TempDir, projects: &TempDir) {
    cq::indexer::sync_sources(
        conn,
        &[("main".to_string(), projects.path().to_path_buf())],
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    )
    .unwrap();
}

/// Rows in `messages` that the live (JSON-parsing) Claude body doesn't produce,
/// plus rows it produces that `messages` lacks. Zero means the materialized
/// table is an exact multiset match for the live view.
fn messages_diff_vs_live(conn: &duckdb::Connection) -> i64 {
    let live = cq::views::claude_messages_sql();
    conn.query_row(
        &format!(
            "SELECT
                (SELECT count(*) FROM (SELECT * FROM messages EXCEPT ALL SELECT * FROM ({live})))
              + (SELECT count(*) FROM (SELECT * FROM ({live}) EXCEPT ALL SELECT * FROM messages))"
        ),
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn compose_claude(conn: &duckdb::Connection) {
    let claude = cq::claude_provider::ClaudeProvider::new_with_base(PathBuf::new());
    cq::views::compose_views(conn, &[&claude]).unwrap();
}

fn materialized_count(conn: &duckdb::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM messages_rows", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn messages_are_materialized_at_index_time() {
    let cache = cache_dir();
    let projects = setup_projects(MATERIALIZE_FIXTURES);
    let conn = cq::cache::open(cache.path(), false).unwrap();
    sync_main(&conn, &cache, &projects);
    compose_claude(&conn);

    assert!(materialized_count(&conn) > 0);
    assert_eq!(messages_diff_vs_live(&conn), 0);
}

#[test]
fn changed_file_replaces_its_materialized_messages() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();
    sync_main(&conn, &cache, &projects);
    let before = materialized_count(&conn);

    // Doubling the file's records doubles its live rows; stale rows left behind
    // by the re-index would show up as a surplus.
    let file = projects
        .path()
        .join("-Users-test-myproject/simple_session.jsonl");
    let original = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{original}{original}")).unwrap();
    sync_main(&conn, &cache, &projects);
    compose_claude(&conn);

    assert!(materialized_count(&conn) > before);
    assert_eq!(messages_diff_vs_live(&conn), 0);
}

#[test]
fn deleted_file_drops_its_materialized_messages() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();
    sync_main(&conn, &cache, &projects);

    std::fs::remove_file(
        projects
            .path()
            .join("-Users-test-myproject/simple_session.jsonl"),
    )
    .unwrap();
    sync_main(&conn, &cache, &projects);
    compose_claude(&conn);

    let leftover: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages_rows WHERE source_file LIKE '%simple_session.jsonl'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
    assert_eq!(messages_diff_vs_live(&conn), 0);
}

/// Files already in a cache keep the `messages_rows` they were indexed with,
/// so a change to the SQL that produces those rows only reaches existing caches
/// through a rebuild. This pins the SQL to the schema version: if it fails,
/// bump `cache::SCHEMA_VERSION`, then update both numbers here.
#[test]
fn materialized_messages_sql_change_requires_schema_bump() {
    let sql = [
        cq::views::claude_messages_over("_index_staging"),
        cq::views::codex_messages_over("_index_staging"),
        cq::views::empty_view_sql(cq::provider::View::Messages).to_string(),
    ]
    .join("\n");
    // FNV-1a: stable across Rust versions, unlike std's DefaultHasher.
    let fingerprint = sql.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    assert_eq!(
        (cq::cache::SCHEMA_VERSION, fingerprint),
        (8, 13539603445860360591),
        "the materialized messages SQL changed: bump cache::SCHEMA_VERSION so existing caches rebuild, then update this test"
    );
}
