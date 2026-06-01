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

/// Sync the cache with the filesystem.
///
/// Behavior depends on `mode`:
/// - `Auto`: check directory mtimes, try-lock, skip if nothing changed or lock busy
/// - `Force`: always scan, wait for lock with timeout
/// - `Skip`: return immediately without touching the database
pub fn sync(
    conn: &Connection,
    projects_dir: &Path,
    mode: SyncMode,
    scope: SyncScope,
    cache_dir: &Path,
) -> Result<SyncResult> {
    // Skip mode: return immediately
    if mode == SyncMode::Skip {
        return Ok(SyncResult {
            stats: SyncStats::default(),
            skipped: true,
            lock_busy: false,
        });
    }

    // Auto mode: check if anything changed via directory mtimes
    if mode == SyncMode::Auto {
        let last_sync = cache::last_sync_at(conn)?;
        let max_mtime = max_dir_mtime(projects_dir, &scope)?;
        if max_mtime <= last_sync {
            let total = count_registry(conn)?;
            return Ok(SyncResult {
                stats: SyncStats { total, ..Default::default() },
                skipped: false,
                lock_busy: false,
            });
        }
    }

    // Acquire file lock
    let lock_path = cache_dir.join("index.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .context("Failed to open lock file")?;

    let lock_acquired = match mode {
        SyncMode::Auto => {
            lock_file.try_lock_exclusive().is_ok()
        }
        SyncMode::Force => {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if lock_file.try_lock_exclusive().is_ok() {
                    break true;
                }
                if std::time::Instant::now() >= deadline {
                    break false;
                }
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
        return Ok(SyncResult {
            stats: SyncStats { total, ..Default::default() },
            skipped: true,
            lock_busy: true,
        });
    }

    // Lock acquired, do the full scan
    let stats = do_sync(conn, projects_dir, &scope)?;

    // Update last_sync_at on success
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    cache::set_last_sync_at(conn, now_ns)?;

    // Release lock
    let _ = lock_file.unlock();

    Ok(SyncResult {
        stats,
        skipped: false,
        lock_busy: false,
    })
}

/// The actual sync logic, extracted so the lock wraps it cleanly.
fn do_sync(conn: &Connection, projects_dir: &Path, scope: &SyncScope) -> Result<SyncStats> {
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
        index_files(conn, &to_add)?;
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
            if let Ok(meta) = std::fs::metadata(f) {
                let mtime = meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                return Ok(mtime);
            }
            return Ok(0);
        }
    };

    let mut max_mtime: i64 = 0;
    for dir in &dirs_to_check {
        if let Ok(meta) = std::fs::metadata(dir) {
            let mtime = meta.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            if mtime > max_mtime {
                max_mtime = mtime;
            }
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

/// Parse JSONL files with DuckDB's read_json and insert into raw_records.
/// Also extracts cwd and registers files in file_registry.
fn index_files(conn: &Connection, files: &[PathBuf]) -> Result<()> {
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
            "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type) VALUES (?, ?, ?, ?, ?)",
            duckdb::params![path_str, mtime_ns, file_size, cwd, agent_type],
        )?;
    }
    Ok(())
}
