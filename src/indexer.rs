use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use duckdb::Connection;

#[derive(Debug, Default)]
pub struct SyncStats {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

struct FileInfo {
    mtime_ns: i64,
    file_size: i64,
}

/// Sync the cache with the filesystem. Scans all JSONL files under
/// projects_dir, diffs against file_registry, and updates raw_records.
pub fn sync(conn: &Connection, projects_dir: &Path) -> Result<SyncStats> {
    let disk_files = scan_filesystem(projects_dir)?;
    let registry = load_registry(conn)?;

    let mut stats = SyncStats::default();
    let mut to_add: Vec<PathBuf> = Vec::new();
    let mut to_remove: Vec<String> = Vec::new();

    // Find new and changed files
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

    // Find deleted files
    for path_str in registry.keys() {
        let path = PathBuf::from(path_str);
        if !disk_files.contains_key(&path) {
            to_remove.push(path_str.clone());
            stats.removed += 1;
        }
    }

    // Apply removals
    for path_str in &to_remove {
        conn.execute("DELETE FROM raw_records WHERE source_file = ?", [path_str])?;
        conn.execute("DELETE FROM file_registry WHERE file_path = ?", [path_str])?;
    }

    // Apply additions
    if !to_add.is_empty() {
        index_files(conn, &to_add)?;
    }

    Ok(stats)
}

/// Scan the filesystem for all JSONL files under the projects directory.
fn scan_filesystem(projects_dir: &Path) -> Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();

    if !projects_dir.exists() {
        return Ok(files);
    }

    for project_entry in std::fs::read_dir(projects_dir)?.filter_map(|e| e.ok()) {
        if !project_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        for file_entry in std::fs::read_dir(project_entry.path())?
            .filter_map(|e| e.ok())
        {
            let path = file_entry.path();
            if path.extension().map(|e| e == "jsonl").unwrap_or(false) && path.is_file() {
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
    }

    Ok(files)
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

        // Insert raw records from this file
        let insert_sql = format!(
            "INSERT INTO raw_records (source_file, json)
             SELECT '{escaped}', CAST(json AS JSON)
             FROM read_json('{escaped}', format='newline_delimited', records=false, ignore_errors=true)"
        );
        conn.execute_batch(&insert_sql)
            .with_context(|| format!("Failed to index {path_str}"))?;

        // Extract cwd from first record that has one
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

        // Get file metadata for registry
        let metadata = std::fs::metadata(file)?;
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let file_size = metadata.len() as i64;

        conn.execute(
            "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd) VALUES (?, ?, ?, ?)",
            duckdb::params![path_str, mtime_ns, file_size, cwd],
        )?;
    }

    Ok(())
}
