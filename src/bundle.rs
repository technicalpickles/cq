//! Core logic for `cq bundle`: packaging one session's raw transcript files
//! (plus best-effort persistedOutputPath sidecars) into a zip. Kept free of
//! any DuckDB dependency so the logic here is unit-testable without a
//! database -- `commands/bundle.rs` is the only place that touches
//! `Connection`.
//!
//! See `docs/specs/2026-09-15-session-bundle-design.md`.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

/// Session-level fields pulled from the `sessions` view for the manifest.
/// Field names deliberately match that view's own columns rather than
/// inventing parallel names.
#[derive(Debug, Default, Clone)]
pub struct SessionMeta {
    pub project: Option<String>,
    pub source: Option<String>,
    pub harness: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    session_id: String,
    project: Option<String>,
    source: Option<String>,
    harness: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    files: Vec<String>,
    sidecars_included: Vec<String>,
    sidecars_missing: Vec<String>,
    cq_version: String,
}

/// What `write_bundle` actually did, for the CLI layer to report.
#[derive(Debug, Default)]
pub struct BundleSummary {
    pub files_written: usize,
    pub sidecars_included: Vec<String>,
    pub sidecars_missing: Vec<String>,
    pub bytes_written: u64,
}

/// The zip-internal path for one discovered session file: `main.jsonl` for
/// the top-level `<session_id>.jsonl`, or `subagents/...` (preserving
/// nesting, including workflow subdirectories) for anything under a
/// `subagents/` directory.
pub fn zip_relative_path(file: &Path) -> String {
    let components: Vec<&std::ffi::OsStr> = file.components().map(|c| c.as_os_str()).collect();
    match components.iter().position(|c| *c == "subagents") {
        Some(idx) => components[idx..]
            .iter()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        None => "main.jsonl".to_string(),
    }
}

/// The sibling `<stem>.meta.json` next to a subagent transcript, if it
/// exists. Mirrors the sidecar lookup `indexer::read_agent_meta` uses, but
/// only needs the path -- the raw file gets copied into the bundle as-is,
/// not parsed.
pub fn meta_sidecar_for(file: &Path) -> Option<PathBuf> {
    let stem = file.file_stem()?.to_str()?;
    let meta = file.with_file_name(format!("{stem}.meta.json"));
    meta.exists().then_some(meta)
}

/// Build the zip at `output`: `files` (already discovered -- main +
/// subagents, `journal.jsonl` already excluded by the caller's discovery
/// step), each file's `.meta.json` sidecar if present, best-effort
/// `persistedOutputPath` sidecars, and `manifest.json`.
pub fn write_bundle(
    session_id: &str,
    meta: SessionMeta,
    files: &[PathBuf],
    output: &Path,
) -> Result<BundleSummary> {
    let zip_file =
        File::create(output).with_context(|| format!("Failed to create {}", output.display()))?;
    let mut zip = zip::ZipWriter::new(zip_file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut manifest_files = Vec::new();
    let mut sidecar_paths = BTreeSet::new();

    for file in files {
        let zip_path = zip_relative_path(file);
        write_file_entry(&mut zip, &zip_path, file, options)?;
        manifest_files.push(zip_path.clone());

        if let Some(meta_path) = meta_sidecar_for(file) {
            let zip_meta_path = zip_path.replace(".jsonl", ".meta.json");
            write_file_entry(&mut zip, &zip_meta_path, &meta_path, options)?;
            manifest_files.push(zip_meta_path);
        }

        sidecar_paths.extend(scan_persisted_output_paths(file)?);
    }

    let mut sidecars_included = Vec::new();
    let mut sidecars_missing = Vec::new();
    let basename_counts = sidecar_paths
        .iter()
        .fold(BTreeMap::new(), |mut counts, path| {
            let basename = Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "sidecar".to_string());
            *counts.entry(basename).or_insert(0usize) += 1;
            counts
        });
    for path in &sidecar_paths {
        let sidecar = Path::new(path);
        if !sidecar.is_file() {
            sidecars_missing.push(path.clone());
            continue;
        }
        let zip_path = sidecar_zip_path(sidecar, &basename_counts);
        write_file_entry(&mut zip, &zip_path, sidecar, options)?;
        sidecars_included.push(zip_path);
    }

    let manifest = Manifest {
        session_id: session_id.to_string(),
        project: meta.project,
        source: meta.source,
        harness: meta.harness,
        started_at: meta.started_at,
        ended_at: meta.ended_at,
        files: manifest_files.clone(),
        sidecars_included: sidecars_included.clone(),
        sidecars_missing: sidecars_missing.clone(),
        cq_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).context("Failed to serialize manifest.json")?;
    zip.start_file("manifest.json", options)
        .context("Failed to start manifest.json in zip")?;
    zip.write_all(&manifest_json)
        .context("Failed to write manifest.json")?;

    zip.finish().context("Failed to finalize zip")?;
    let bytes_written = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    Ok(BundleSummary {
        files_written: manifest_files.len(),
        sidecars_included,
        sidecars_missing,
        bytes_written,
    })
}

fn write_file_entry(
    zip: &mut zip::ZipWriter<File>,
    zip_path: &str,
    source: &Path,
    options: zip::write::SimpleFileOptions,
) -> Result<()> {
    let mut contents = Vec::new();
    File::open(source)
        .with_context(|| format!("Failed to open {}", source.display()))?
        .read_to_end(&mut contents)
        .with_context(|| format!("Failed to read {}", source.display()))?;
    zip.start_file(zip_path, options)
        .with_context(|| format!("Failed to start {zip_path} in zip"))?;
    zip.write_all(&contents)
        .with_context(|| format!("Failed to write {zip_path} to zip"))?;
    Ok(())
}

/// Scan one JSONL file for `toolUseResult.persistedOutputPath` pointers.
/// Unparseable lines are skipped, not errors -- a bundle is a best-effort
/// artifact over real transcripts, which already tolerate the same
/// unparseable-line reality every other cq command works around (see
/// `docs/session-storage.md`). Returns paths deduplicated and sorted.
pub fn scan_persisted_output_paths(file: &Path) -> Result<Vec<String>> {
    let handle = File::open(file)
        .with_context(|| format!("Failed to open {} for sidecar scan", file.display()))?;
    let mut found = BTreeSet::new();
    for line in BufReader::new(handle).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(path) = value
            .get("toolUseResult")
            .and_then(|v| v.get("persistedOutputPath"))
            .and_then(|v| v.as_str())
        {
            found.insert(path.to_string());
        }
    }
    Ok(found.into_iter().collect())
}

fn sidecar_zip_path(sidecar: &Path, basename_counts: &BTreeMap<String, usize>) -> String {
    let basename = sidecar
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "sidecar".to_string());

    if basename_counts.get(&basename).copied().unwrap_or(0) <= 1 {
        return format!("sidecars/{basename}");
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sidecar.to_string_lossy().hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());

    match basename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
            format!("sidecars/{stem}-{hash}.{ext}")
        }
        _ => format!("sidecars/{basename}-{hash}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn zip_relative_path_for_main_file() {
        let path = Path::new("/home/x/.claude/projects/-Users-x-proj/abc-123.jsonl");
        assert_eq!(zip_relative_path(path), "main.jsonl");
    }

    #[test]
    fn zip_relative_path_for_plain_subagent() {
        let path =
            Path::new("/home/x/.claude/projects/-Users-x-proj/abc-123/subagents/agent-sub1.jsonl");
        assert_eq!(zip_relative_path(path), "subagents/agent-sub1.jsonl");
    }

    #[test]
    fn zip_relative_path_for_workflow_subagent() {
        let path = Path::new(
            "/home/x/.claude/projects/-Users-x-proj/abc-123/subagents/workflows/wf_1/agent-wf.jsonl",
        );
        assert_eq!(
            zip_relative_path(path),
            "subagents/workflows/wf_1/agent-wf.jsonl"
        );
    }

    #[test]
    fn meta_sidecar_for_existing_sidecar() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("agent-sub1.jsonl");
        std::fs::write(&jsonl, "{}").unwrap();
        let meta = dir.path().join("agent-sub1.meta.json");
        std::fs::write(&meta, "{}").unwrap();
        assert_eq!(meta_sidecar_for(&jsonl), Some(meta));
    }

    #[test]
    fn meta_sidecar_for_missing_sidecar() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("agent-sub1.jsonl");
        std::fs::write(&jsonl, "{}").unwrap();
        assert_eq!(meta_sidecar_for(&jsonl), None);
    }

    #[test]
    fn scan_finds_persisted_output_path() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(
            &jsonl,
            "{\"type\":\"user\",\"toolUseResult\":{\"persistedOutputPath\":\"/tmp/out.txt\"}}\n\
             {\"type\":\"assistant\"}\n",
        )
        .unwrap();
        assert_eq!(
            scan_persisted_output_paths(&jsonl).unwrap(),
            vec!["/tmp/out.txt".to_string()]
        );
    }

    #[test]
    fn scan_skips_unparseable_and_blank_lines() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(&jsonl, "not json at all\n\n").unwrap();
        assert_eq!(
            scan_persisted_output_paths(&jsonl).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn write_bundle_zips_files_meta_and_manifest() {
        let src_dir = TempDir::new().unwrap();
        let session_id = "test-session-id";
        let main_file = src_dir.path().join(format!("{session_id}.jsonl"));
        std::fs::write(&main_file, "{\"type\":\"user\"}\n").unwrap();

        let sub_dir = src_dir.path().join(session_id).join("subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let sub_file = sub_dir.join("agent-sub1.jsonl");
        std::fs::write(&sub_file, "{\"type\":\"assistant\"}\n").unwrap();
        std::fs::write(
            sub_dir.join("agent-sub1.meta.json"),
            "{\"agentType\":\"general-purpose\"}",
        )
        .unwrap();

        let out_dir = TempDir::new().unwrap();
        let out_path = out_dir.path().join("bundle.zip");

        let meta = SessionMeta {
            project: Some("myproject".to_string()),
            source: Some("main".to_string()),
            harness: Some("claude".to_string()),
            started_at: Some("2026-09-10T12:00:00Z".to_string()),
            ended_at: Some("2026-09-10T12:05:00Z".to_string()),
        };

        let summary = write_bundle(session_id, meta, &[main_file, sub_file], &out_path).unwrap();

        assert_eq!(summary.files_written, 3); // main + subagent + its meta.json
        assert!(summary.sidecars_included.is_empty());
        assert!(summary.sidecars_missing.is_empty());

        let zip_file = std::fs::File::open(&out_path).unwrap();
        let mut archive = zip::ZipArchive::new(zip_file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"main.jsonl".to_string()), "{names:?}");
        assert!(
            names.contains(&"subagents/agent-sub1.jsonl".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"subagents/agent-sub1.meta.json".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"manifest.json".to_string()), "{names:?}");

        let manifest_idx = names.iter().position(|n| n == "manifest.json").unwrap();
        let mut manifest_str = String::new();
        archive
            .by_index(manifest_idx)
            .unwrap()
            .read_to_string(&mut manifest_str)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
        assert_eq!(manifest["session_id"], session_id);
        assert_eq!(manifest["project"], "myproject");
    }

    #[test]
    fn scan_dedupes_repeated_paths() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(
            &jsonl,
            "{\"toolUseResult\":{\"persistedOutputPath\":\"/tmp/out.txt\"}}\n\
             {\"toolUseResult\":{\"persistedOutputPath\":\"/tmp/out.txt\"}}\n",
        )
        .unwrap();
        assert_eq!(
            scan_persisted_output_paths(&jsonl).unwrap(),
            vec!["/tmp/out.txt".to_string()]
        );
    }

    #[test]
    fn sidecar_zip_path_keeps_unique_basenames_flat() {
        let counts = BTreeMap::from([("out.txt".to_string(), 1usize)]);
        assert_eq!(
            sidecar_zip_path(Path::new("/tmp/out.txt"), &counts),
            "sidecars/out.txt"
        );
    }

    #[test]
    fn sidecar_zip_path_disambiguates_duplicate_basenames() {
        let counts = BTreeMap::from([("out.txt".to_string(), 2usize)]);
        let left = sidecar_zip_path(Path::new("/tmp/a/out.txt"), &counts);
        let right = sidecar_zip_path(Path::new("/tmp/b/out.txt"), &counts);
        assert!(left.starts_with("sidecars/out-") && left.ends_with(".txt"));
        assert!(right.starts_with("sidecars/out-") && right.ends_with(".txt"));
        assert_ne!(left, right);
    }
}
