use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use duckdb::Connection;
use crate::provider::{TranscriptProvider, ProjectInfo};
use crate::scope::QueryScope;
use crate::views;

pub struct ClaudeProvider {
    base_dir: PathBuf,
}

impl ClaudeProvider {
    pub fn new() -> Result<Self> {
        let base_dir = if let Ok(dir) = std::env::var("CQ_PROJECTS_DIR") {
            PathBuf::from(dir)
        } else {
            let home = dirs::home_dir().context("Could not determine home directory")?;
            home.join(".claude").join("projects")
        };
        Ok(Self { base_dir })
    }

    pub fn new_with_base(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn encode_path(path: &str) -> String {
        path.replace('/', "-").replace('.', "-")
    }

    pub fn decode_path(encoded: &str) -> String {
        if encoded.starts_with('-') {
            format!("/{}", encoded[1..].replace('-', "/"))
        } else {
            encoded.replace('-', "/")
        }
    }

    fn matches_project(&self, encoded_name: &str, query: &str) -> bool {
        let decoded = Self::decode_path(encoded_name);
        decoded.to_lowercase().contains(&query.to_lowercase())
    }
}

impl TranscriptProvider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn discover_files(&self, scope: &QueryScope) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        if !self.base_dir.exists() {
            return Ok(files);
        }

        let entries: Vec<_> = std::fs::read_dir(&self.base_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();

        for entry in entries {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if let Some(ref project) = scope.project {
                if !self.matches_project(&dir_name, project) {
                    continue;
                }
            }
            if let Ok(dir_entries) = std::fs::read_dir(entry.path()) {
                for file_entry in dir_entries.filter_map(|e| e.ok()) {
                    let path = file_entry.path();
                    if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                        if let Some(ref session) = scope.session {
                            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                            if !stem.starts_with(session.as_str()) {
                                continue;
                            }
                        }
                        files.push(path);
                    }
                }
            }
        }
        Ok(files)
    }

    fn register_views(&self, conn: &Connection, files: &[PathBuf]) -> Result<()> {
        views::register_views(conn, files)
    }

    fn list_projects(&self) -> Result<Vec<ProjectInfo>> {
        let mut projects = Vec::new();
        if !self.base_dir.exists() {
            return Ok(projects);
        }

        for entry in std::fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let encoded_name = entry.file_name().to_string_lossy().to_string();
            let decoded_path = Self::decode_path(&encoded_name);
            let file_count = std::fs::read_dir(entry.path())?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|ext| ext == "jsonl").unwrap_or(false))
                .count();
            projects.push(ProjectInfo {
                encoded_name,
                decoded_path,
                file_count,
            });
        }
        projects.sort_by(|a, b| b.file_count.cmp(&a.file_count));
        Ok(projects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn encode_path() {
        assert_eq!(
            ClaudeProvider::encode_path("/Users/josh/pickleton"),
            "-Users-josh-pickleton"
        );
    }

    #[test]
    fn encode_path_with_dots() {
        assert_eq!(
            ClaudeProvider::encode_path("/Users/josh.nichols/my.project"),
            "-Users-josh-nichols-my-project"
        );
    }

    #[test]
    fn decode_path() {
        assert_eq!(
            ClaudeProvider::decode_path("-Users-josh-pickleton"),
            "/Users/josh/pickleton"
        );
    }

    #[test]
    fn project_matching_substring() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-josh-pickleton");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc12345-0000-0000-0000-000000000000.jsonl"),
            "{}",
        )
        .unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let scope = QueryScope::new(Some("pickleton".to_string()), None, None);
        let files = provider.discover_files(&scope).unwrap();
        assert_eq!(files.len(), 1);

        let projects = provider.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].decoded_path.contains("pickleton"));
    }

    #[test]
    fn session_prefix_filtering() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-josh-pickleton");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc12345-6789-0000-0000-000000000000.jsonl"),
            "{}",
        )
        .unwrap();
        fs::write(
            project_dir.join("def99999-1111-2222-3333-444444444444.jsonl"),
            "{}",
        )
        .unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let scope = QueryScope::new(None, Some("abc123".to_string()), None);
        let files = provider.discover_files(&scope).unwrap();
        assert_eq!(files.len(), 1);
        let filename = files[0].file_name().unwrap().to_string_lossy();
        assert!(filename.starts_with("abc12345"));
    }

    #[test]
    fn discover_files_no_filter_returns_all() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-josh-pickleton");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc12345-6789-0000-0000-000000000000.jsonl"),
            "{}",
        )
        .unwrap();
        fs::write(
            project_dir.join("def99999-1111-2222-3333-444444444444.jsonl"),
            "{}",
        )
        .unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let scope = QueryScope::new(None, None, None);
        let files = provider.discover_files(&scope).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn project_filter_excludes_nonmatching() {
        let tmp = TempDir::new().unwrap();
        let project_a = tmp.path().join("-Users-josh-pickleton");
        let project_b = tmp.path().join("-Users-josh-other-repo");
        fs::create_dir_all(&project_a).unwrap();
        fs::create_dir_all(&project_b).unwrap();
        fs::write(project_a.join("session1.jsonl"), "{}").unwrap();
        fs::write(project_b.join("session2.jsonl"), "{}").unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let scope = QueryScope::new(Some("pickleton".to_string()), None, None);
        let files = provider.discover_files(&scope).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].to_string_lossy().contains("pickleton"));
    }

    #[test]
    fn empty_base_dir_returns_empty() {
        let provider = ClaudeProvider::new_with_base(PathBuf::from("/nonexistent/path/that/does/not/exist"));
        let scope = QueryScope::new(None, None, None);
        let files = provider.discover_files(&scope).unwrap();
        assert!(files.is_empty());
        let projects = provider.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn list_projects_sorted_by_file_count() {
        let tmp = TempDir::new().unwrap();
        let project_a = tmp.path().join("-Users-josh-few");
        let project_b = tmp.path().join("-Users-josh-many");
        fs::create_dir_all(&project_a).unwrap();
        fs::create_dir_all(&project_b).unwrap();
        fs::write(project_a.join("s1.jsonl"), "{}").unwrap();
        fs::write(project_b.join("s1.jsonl"), "{}").unwrap();
        fs::write(project_b.join("s2.jsonl"), "{}").unwrap();
        fs::write(project_b.join("s3.jsonl"), "{}").unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let projects = provider.list_projects().unwrap();
        assert_eq!(projects.len(), 2);
        // sorted by file_count descending
        assert!(projects[0].file_count >= projects[1].file_count);
        assert_eq!(projects[0].file_count, 3);
    }
}
