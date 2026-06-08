use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use duckdb::Connection;
use fs2::FileExt;

use crate::cache;
use crate::db::SyncMode;
use crate::sync_scope::SyncScope;

#[derive(Debug, Default)]
pub struct SyncStats {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub total: usize,
}

/// Result of a sync operation, including whether it was skipped.
#[derive(Debug)]
pub struct SyncResult {
    pub stats: SyncStats,
    /// True if the sync was skipped (Skip mode or lock busy).
    pub skipped: bool,
    /// True if the sync was skipped specifically because the lock was busy.
    pub lock_busy: bool,
}

struct FileInfo {
    mtime_ns: i64,
    file_size: i64,
}

/// Sync the cache against multiple sources. Auto mode checks the max dir mtime
/// across all sources, takes the lock once, syncs each source, and writes
/// last_sync_at a single time so one source's write can't suppress another's scan.
pub fn sync_sources(
    conn: &Connection,
    sources: &[(String, PathBuf)],
    mode: SyncMode,
    scope: SyncScope,
    cache_dir: &Path,
) -> Result<SyncResult> {
    if mode == SyncMode::Skip {
        return Ok(SyncResult { stats: SyncStats::default(), skipped: true, lock_busy: false });
    }

    if mode == SyncMode::Auto {
        let last_sync = cache::last_sync_at(conn)?;
        let mut max_mtime = 0i64;
        for (_, dir) in sources {
            let m = max_dir_mtime(dir, &scope)?;
            if m > max_mtime { max_mtime = m; }
        }
        if max_mtime <= last_sync {
            let total = count_registry(conn)?;
            return Ok(SyncResult { stats: SyncStats { total, ..Default::default() }, skipped: false, lock_busy: false });
        }
    }

    let lock_path = cache_dir.join("index.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true).write(true).truncate(false).open(&lock_path)
        .context("Failed to open lock file")?;

    let lock_acquired = match mode {
        SyncMode::Auto => lock_file.try_lock_exclusive().is_ok(),
        SyncMode::Force => {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if lock_file.try_lock_exclusive().is_ok() { break true; }
                if std::time::Instant::now() >= deadline { break false; }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        SyncMode::Skip => unreachable!(),
    };

    if !lock_acquired {
        if mode == SyncMode::Force {
            anyhow::bail!("index locked by another process after 5s, try again shortly");
        }
        let total = count_registry(conn)?;
        return Ok(SyncResult { stats: SyncStats { total, ..Default::default() }, skipped: true, lock_busy: true });
    }

    let mut agg = SyncStats::default();
    for (name, dir) in sources {
        // Restrict each source to the project dirs that physically live under it,
        // so a project-scoped sync tags files with their own source (not whichever
        // source's pass ran first). See sync_scope_projects_attributes_per_source.
        let per_source_scope = match &scope {
            SyncScope::All => SyncScope::All,
            SyncScope::Projects(dirs) => SyncScope::Projects(
                dirs.iter().filter(|d| d.starts_with(dir)).cloned().collect()
            ),
            SyncScope::File(f) => {
                if f.starts_with(dir) {
                    SyncScope::File(f.clone())
                } else {
                    // This source owns none of the requested file.
                    SyncScope::Projects(Vec::new())
                }
            }
        };
        let s = do_sync(conn, dir, &per_source_scope, name)?;
        agg.added += s.added;
        agg.removed += s.removed;
        agg.changed += s.changed;
    }
    agg.total = count_registry(conn)?;

    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64).unwrap_or(0);
    cache::set_last_sync_at(conn, now_ns)?;

    let _ = lock_file.unlock();
    Ok(SyncResult { stats: agg, skipped: false, lock_busy: false })
}

/// The actual sync logic, extracted so the lock wraps it cleanly.
fn do_sync(conn: &Connection, projects_dir: &Path, scope: &SyncScope, source_name: &str) -> Result<SyncStats> {
    let disk_files = scan_filesystem(projects_dir, scope)?;
    let registry = load_registry(conn)?;

    let mut stats = SyncStats::default();
    let mut to_add: Vec<PathBuf> = Vec::new();
    let mut to_remove: Vec<String> = Vec::new();

    for (path, info) in &disk_files {
        let path_str = path.to_string_lossy().to_string();
        match registry.get(&path_str) {
            None => {
                to_add.push(path.clone());
                stats.added += 1;
            }
            Some(reg) => {
                if reg.mtime_ns != info.mtime_ns || reg.file_size != info.file_size {
                    to_remove.push(path_str);
                    to_add.push(path.clone());
                    stats.changed += 1;
                }
            }
        }
    }

    for path_str in registry.keys() {
        let path = PathBuf::from(path_str);
        if in_scope(&path, projects_dir, scope) && !disk_files.contains_key(&path) {
            to_remove.push(path_str.clone());
            stats.removed += 1;
        }
    }

    for path_str in &to_remove {
        conn.execute("DELETE FROM raw_records WHERE source_file = ?", [path_str])?;
        conn.execute("DELETE FROM file_registry WHERE file_path = ?", [path_str])?;
    }

    if !to_add.is_empty() {
        index_files(conn, &to_add, source_name)?;
    }

    stats.total = if matches!(scope, SyncScope::All) {
        count_registry(conn)?
    } else {
        disk_files.len()
    };

    Ok(stats)
}

fn in_scope(path: &Path, projects_dir: &Path, scope: &SyncScope) -> bool {
    match scope {
        SyncScope::All => path.starts_with(projects_dir),
        SyncScope::Projects(dirs) => dirs.iter().any(|d| path.starts_with(d)),
        SyncScope::File(f) => path == f,
    }
}

fn max_dir_mtime(projects_dir: &Path, scope: &SyncScope) -> Result<i64> {
    let dirs_to_check: Vec<PathBuf> = match scope {
        SyncScope::All => {
            if !projects_dir.exists() {
                return Ok(0);
            }
            std::fs::read_dir(projects_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        }
        SyncScope::Projects(dirs) => dirs.clone(),
        SyncScope::File(f) => {
            return Ok(dir_mtime(f));
        }
    };

    let mut max_mtime: i64 = 0;
    for dir in &dirs_to_check {
        let m = max_mtime_recursive(dir);
        if m > max_mtime {
            max_mtime = m;
        }
    }

    Ok(max_mtime)
}

fn count_registry(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_registry", [], |r| r.get(0)
    )?;
    Ok(count as usize)
}

fn scan_filesystem(projects_dir: &Path, scope: &SyncScope) -> Result<HashMap<PathBuf, FileInfo>> {
    match scope {
        SyncScope::All => scan_all(projects_dir),
        SyncScope::Projects(dirs) => {
            let mut files = HashMap::new();
            for dir in dirs {
                let dir_files = scan_directory(dir)?;
                files.extend(dir_files);
            }
            Ok(files)
        }
        SyncScope::File(path) => {
            let mut files = HashMap::new();
            if path.is_file() {
                if let Ok(metadata) = std::fs::metadata(path) {
                    let mtime_ns = metadata.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as i64)
                        .unwrap_or(0);
                    files.insert(path.clone(), FileInfo {
                        mtime_ns,
                        file_size: metadata.len() as i64,
                    });
                }
            }
            Ok(files)
        }
    }
}

fn scan_all(projects_dir: &Path) -> Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();
    if !projects_dir.exists() {
        return Ok(files);
    }
    for project_entry in std::fs::read_dir(projects_dir)?.filter_map(|e| e.ok()) {
        if !project_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir_files = scan_directory(&project_entry.path())?;
        files.extend(dir_files);
    }
    Ok(files)
}

fn scan_directory(dir: &Path) -> Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();
    collect_jsonl(dir, &mut files)?;
    Ok(files)
}

/// Recursively collect indexable JSONL transcripts under `dir`.
fn collect_jsonl(dir: &Path, files: &mut HashMap<PathBuf, FileInfo>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_jsonl(&path, files)?;
        } else if is_indexable_jsonl(&path) {
            if let Ok(metadata) = std::fs::metadata(&path) {
                let mtime_ns = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                files.insert(
                    path,
                    FileInfo {
                        mtime_ns,
                        file_size: metadata.len() as i64,
                    },
                );
            }
        }
    }
    Ok(())
}

/// Read `agentType` from the sibling `agent-<id>.meta.json`, if this file is a
/// subagent transcript and the sidecar exists.
fn read_agent_type(file: &Path) -> Option<String> {
    let name = file.file_name()?.to_str()?;
    if !name.starts_with("agent-") {
        return None;
    }
    let stem = file.file_stem()?.to_str()?;
    let meta = file.with_file_name(format!("{stem}.meta.json"));
    let data = std::fs::read_to_string(&meta).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    value.get("agentType")?.as_str().map(|s| s.to_string())
}

/// A `.jsonl` transcript we should index. Excludes workflow `journal.jsonl`
/// ledgers, which are resume bookkeeping, not transcripts.
fn is_indexable_jsonl(path: &Path) -> bool {
    path.extension().map(|e| e == "jsonl").unwrap_or(false)
        && path.file_name().map(|n| n != "journal.jsonl").unwrap_or(false)
}

/// Load the current file registry from the database.
fn load_registry(conn: &Connection) -> Result<HashMap<String, FileInfo>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, mtime_ns, file_size FROM file_registry"
    )?;
    let mut registry = HashMap::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let mtime_ns: i64 = row.get(1)?;
        let file_size: i64 = row.get(2)?;
        registry.insert(path, FileInfo { mtime_ns, file_size });
    }
    Ok(registry)
}

fn dir_mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Max mtime of `dir` and all of its subdirectories (recursively). Creating any
/// new file bumps its immediate parent dir's mtime, so this catches new sessions,
/// subagents, and workflow agents. It does not catch pure appends (no directory
/// mtime changes on append); `--reindex` covers that case.
fn max_mtime_recursive(dir: &Path) -> i64 {
    let mut max = dir_mtime(dir);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let m = max_mtime_recursive(&entry.path());
                if m > max {
                    max = m;
                }
            }
        }
    }
    max
}

/// Parse JSONL files with DuckDB's read_json and insert into raw_records.
/// Also extracts cwd and registers files in file_registry.
fn index_files(conn: &Connection, files: &[PathBuf], source_name: &str) -> Result<()> {
    for file in files {
        let path_str = file.to_string_lossy().to_string();
        let escaped = path_str.replace('\'', "''");

        let insert_sql = format!(
            "INSERT INTO raw_records (source_file, json)
             SELECT '{escaped}', CAST(json AS JSON)
             FROM read_json('{escaped}', format='newline_delimited', records=false, ignore_errors=true)"
        );
        conn.execute_batch(&insert_sql)
            .with_context(|| format!("Failed to index {path_str}"))?;

        let cwd: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT json_extract_string(json, '$.cwd')
                     FROM raw_records
                     WHERE source_file = '{escaped}'
                     AND json_extract_string(json, '$.cwd') IS NOT NULL
                     LIMIT 1"
                ),
                [],
                |r| r.get(0),
            )
            .ok();

        let agent_type = read_agent_type(file);

        let metadata = std::fs::metadata(file)?;
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let file_size = metadata.len() as i64;

        conn.execute(
            "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type, source) VALUES (?, ?, ?, ?, ?, ?)",
            duckdb::params![path_str, mtime_ns, file_size, cwd, agent_type, source_name],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_scope::SyncScope;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn sync_sources_records_source_name() {
        use crate::cache;
        let cache_tmp = TempDir::new().unwrap();
        let conn = cache::open(cache_tmp.path(), true).unwrap();

        let main_tmp = TempDir::new().unwrap();
        let env_tmp = TempDir::new().unwrap();
        let main_proj = main_tmp.path().join("-Users-josh-a");
        let env_proj = env_tmp.path().join("-Users-josh-b");
        std::fs::create_dir_all(&main_proj).unwrap();
        std::fs::create_dir_all(&env_proj).unwrap();
        std::fs::write(main_proj.join("s1.jsonl"), "{\"cwd\":\"/x\"}\n").unwrap();
        std::fs::write(env_proj.join("s2.jsonl"), "{\"cwd\":\"/y\"}\n").unwrap();

        let sources = vec![
            ("main".to_string(), main_tmp.path().to_path_buf()),
            ("pinwheel".to_string(), env_tmp.path().to_path_buf()),
        ];
        sync_sources(&conn, &sources, SyncMode::Force, SyncScope::All, cache_tmp.path()).unwrap();

        let n_pinwheel: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_registry WHERE source = 'pinwheel'", [], |r| r.get(0))
            .unwrap();
        let n_main: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_registry WHERE source = 'main'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_pinwheel, 1);
        assert_eq!(n_main, 1);
    }

    #[test]
    fn sync_scope_projects_attributes_per_source() {
        // Two sources each have a project dir of the SAME encoded name. Under a
        // Projects scope that unions both dirs (as project_dirs_for_query would),
        // each file must be tagged with its OWN source, not whichever ran first.
        use crate::cache;
        let cache_tmp = TempDir::new().unwrap();
        let conn = cache::open(cache_tmp.path(), true).unwrap();

        let main_tmp = TempDir::new().unwrap();
        let env_tmp = TempDir::new().unwrap();
        let main_proj = main_tmp.path().join("-Users-josh-shared");
        let env_proj = env_tmp.path().join("-Users-josh-shared");
        std::fs::create_dir_all(&main_proj).unwrap();
        std::fs::create_dir_all(&env_proj).unwrap();
        std::fs::write(main_proj.join("s1.jsonl"), "{\"cwd\":\"/x\"}\n").unwrap();
        std::fs::write(env_proj.join("s2.jsonl"), "{\"cwd\":\"/x\"}\n").unwrap();

        let sources = vec![
            ("main".to_string(), main_tmp.path().to_path_buf()),
            ("pinwheel".to_string(), env_tmp.path().to_path_buf()),
        ];
        // Union of both project dirs, the shape project_dirs_for_query produces.
        let scope = SyncScope::Projects(vec![main_proj.clone(), env_proj.clone()]);
        sync_sources(&conn, &sources, SyncMode::Force, scope, cache_tmp.path()).unwrap();

        let n_main: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_registry WHERE source = 'main'", [], |r| r.get(0))
            .unwrap();
        let n_pinwheel: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_registry WHERE source = 'pinwheel'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_main, 1, "main's file must be tagged 'main', not the other source");
        assert_eq!(n_pinwheel, 1, "pinwheel's file must be tagged 'pinwheel', not 'main'");
    }

    #[test]
    fn max_dir_mtime_detects_deep_new_file() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("-Users-test-myproject");
        let sub = proj.join("sess-1").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();

        let scope = SyncScope::All;
        let before = max_dir_mtime(tmp.path(), &scope).unwrap();

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(sub.join("agent-new.jsonl"), "{}\n").unwrap();

        let after = max_dir_mtime(tmp.path(), &scope).unwrap();
        assert!(
            after > before,
            "a new deep subagent file must advance max_dir_mtime (before={before}, after={after})"
        );
    }
}
