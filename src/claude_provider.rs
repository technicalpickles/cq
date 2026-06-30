use crate::provider::{ProjectInfo, TranscriptProvider};
use crate::scope::QueryScope;
use crate::source::{self, Source};
use crate::views;
use anyhow::Result;
use duckdb::Connection;
use std::path::{Path, PathBuf};

pub struct ClaudeProvider {
    sources: Vec<Source>,
}

impl ClaudeProvider {
    /// Build the full source list: main + discovered cenv envs.
    pub fn new() -> Result<Self> {
        let mut sources = vec![source::main_source()?];
        sources.extend(source::discover_cenv_sources());
        Ok(Self { sources })
    }

    /// Construct from an explicit source list (tests, and future config-listed sources).
    pub fn with_sources(sources: Vec<Source>) -> Self {
        Self { sources }
    }

    /// Single-source constructor kept for existing tests. Names the source `main`.
    pub fn new_with_base(base_dir: PathBuf) -> Self {
        Self {
            sources: vec![Source {
                name: "main".to_string(),
                projects_dir: base_dir,
            }],
        }
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// The directories the indexer should scan, one per source.
    pub fn projects_dirs(&self) -> Vec<PathBuf> {
        self.sources
            .iter()
            .map(|s| s.projects_dir.clone())
            .collect()
    }

    /// The source name a transcript file belongs to: the source whose
    /// `projects_dir` is a prefix of the file path. None if no source matches.
    pub fn source_for_file(&self, file: &Path) -> Option<String> {
        self.sources
            .iter()
            .find(|s| file.starts_with(&s.projects_dir))
            .map(|s| s.name.clone())
    }

    /// The source whose `projects_dir` lies under `config_dir` (the env dir from
    /// CLAUDE_CONFIG_DIR), if any. Used to pick the active source.
    pub fn source_for_config_dir(&self, config_dir: &Path) -> Option<String> {
        self.sources
            .iter()
            .find(|s| s.projects_dir.starts_with(config_dir))
            .map(|s| s.name.clone())
    }

    pub fn encode_path(path: &str) -> String {
        path.replace(['/', '.'], "-")
    }

    pub fn decode_path(encoded: &str) -> String {
        if let Some(rest) = encoded.strip_prefix('-') {
            format!("/{}", rest.replace('-', "/"))
        } else {
            encoded.replace('-', "/")
        }
    }

    fn matches_project(encoded_name: &str, query: &str) -> bool {
        let decoded = Self::decode_path(encoded_name);
        decoded.to_lowercase().contains(&query.to_lowercase())
    }

    /// Project dirs (across all sources) whose decoded name matches the query.
    /// Used by SyncScope::Projects.
    pub fn project_dirs_for_query(&self, query: &str) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for src in &self.sources {
            let Ok(entries) = std::fs::read_dir(&src.projects_dir) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().to_string();
                if Self::matches_project(&dir_name, query) {
                    dirs.push(entry.path());
                }
            }
        }
        dirs
    }

    /// Find a project path matching `cwd` (or a parent) in ANY source.
    pub fn project_for_cwd(&self, cwd: &str) -> Option<String> {
        let mut path = std::path::Path::new(cwd);
        loop {
            let encoded = Self::encode_path(&path.to_string_lossy());
            for src in &self.sources {
                if src.projects_dir.join(&encoded).is_dir() {
                    return Some(path.to_string_lossy().to_string());
                }
            }
            match path.parent() {
                Some(parent) if parent != path => path = parent,
                _ => break,
            }
        }
        None
    }
}

impl TranscriptProvider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn discover_files(&self, scope: &QueryScope) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for src in &self.sources {
            if let Some(ref want) = scope.source {
                if &src.name != want {
                    continue;
                }
            }
            if !src.projects_dir.exists() {
                continue;
            }
            let project_dirs = std::fs::read_dir(&src.projects_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false));
            for entry in project_dirs {
                let dir_name = entry.file_name().to_string_lossy().to_string();
                if let Some(ref project) = scope.project {
                    if !Self::matches_project(&dir_name, project) {
                        continue;
                    }
                }
                let project_path = entry.path();
                let mut found = Vec::new();
                collect_jsonl_paths(&project_path, &mut found);
                for path in found {
                    if let Some(ref session) = scope.session {
                        let sid = session_id_for_file(&project_path, &path);
                        if !sid.starts_with(session.as_str()) {
                            continue;
                        }
                    }
                    files.push(path);
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
        for src in &self.sources {
            let Ok(entries) = std::fs::read_dir(&src.projects_dir) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let encoded_name = entry.file_name().to_string_lossy().to_string();
                let decoded_path = Self::decode_path(&encoded_name);
                let file_count = std::fs::read_dir(entry.path())
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| {
                                e.path()
                                    .extension()
                                    .map(|ext| ext == "jsonl")
                                    .unwrap_or(false)
                            })
                            .count()
                    })
                    .unwrap_or(0);
                projects.push(ProjectInfo {
                    encoded_name,
                    decoded_path,
                    file_count,
                });
            }
        }
        projects.sort_by_key(|p| std::cmp::Reverse(p.file_count));
        Ok(projects)
    }

    fn prepare(&self, _conn: &Connection) -> Result<bool> {
        // Claude's JSONL->raw_records sync happens in db::setup_connection /
        // indexer; nothing extra to prepare here. Always active.
        Ok(true)
    }

    fn contribute_view_sql(&self, view: crate::provider::View) -> Option<String> {
        use crate::provider::View;
        Some(match view {
            View::Messages => crate::views::claude_messages_sql(),
            View::ToolCalls => crate::views::claude_tool_calls_sql(),
            View::ToolResults => crate::views::claude_tool_results_sql(),
            View::Sessions => crate::views::claude_sessions_sql(),
        })
    }
}

/// Recursively collect indexable `.jsonl` files under `dir`, excluding
/// workflow `journal.jsonl` ledgers.
fn collect_jsonl_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_jsonl_paths(&path, out);
        } else if path.extension().map(|e| e == "jsonl").unwrap_or(false)
            && path
                .file_name()
                .map(|n| n != "journal.jsonl")
                .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// The session a transcript belongs to: the first path component under the
/// project dir, with any `.jsonl` extension stripped. For a top-level file
/// `<project>/<uuid>.jsonl` this is `<uuid>`; for a nested subagent file
/// `<project>/<uuid>/subagents/agent-x.jsonl` it is also `<uuid>`.
fn session_id_for_file(project_dir: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(project_dir).unwrap_or(file);
    match rel.components().next() {
        Some(first) => {
            let s = first.as_os_str().to_string_lossy();
            s.strip_suffix(".jsonl").unwrap_or(&s).to_string()
        }
        None => String::new(),
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
            ClaudeProvider::encode_path("/Users/alice/myproject"),
            "-Users-alice-myproject"
        );
    }

    #[test]
    fn encode_path_with_dots() {
        assert_eq!(
            ClaudeProvider::encode_path("/Users/alice.smith/my.project"),
            "-Users-alice-smith-my-project"
        );
    }

    #[test]
    fn decode_path() {
        assert_eq!(
            ClaudeProvider::decode_path("-Users-alice-myproject"),
            "/Users/alice/myproject"
        );
    }

    #[test]
    fn project_matching_substring() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-alice-myproject");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("abc12345-0000-0000-0000-000000000000.jsonl"),
            "{}",
        )
        .unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let scope = QueryScope::new(Some("myproject".to_string()), None, None);
        let files = provider.discover_files(&scope).unwrap();
        assert_eq!(files.len(), 1);

        let projects = provider.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].decoded_path.contains("myproject"));
    }

    #[test]
    fn session_prefix_filtering() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-alice-myproject");
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
        let project_dir = tmp.path().join("-Users-alice-myproject");
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
        let project_a = tmp.path().join("-Users-alice-myproject");
        let project_b = tmp.path().join("-Users-josh-other-repo");
        fs::create_dir_all(&project_a).unwrap();
        fs::create_dir_all(&project_b).unwrap();
        fs::write(project_a.join("session1.jsonl"), "{}").unwrap();
        fs::write(project_b.join("session2.jsonl"), "{}").unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let scope = QueryScope::new(Some("myproject".to_string()), None, None);
        let files = provider.discover_files(&scope).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].to_string_lossy().contains("myproject"));
    }

    #[test]
    fn empty_base_dir_returns_empty() {
        let provider =
            ClaudeProvider::new_with_base(PathBuf::from("/nonexistent/path/that/does/not/exist"));
        let scope = QueryScope::new(None, None, None);
        let files = provider.discover_files(&scope).unwrap();
        assert!(files.is_empty());
        let projects = provider.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn project_for_cwd_exact_match() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-test-myproject");
        fs::create_dir_all(&project_dir).unwrap();
        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let result = provider.project_for_cwd("/Users/test/myproject");
        assert_eq!(result, Some("/Users/test/myproject".to_string()));
    }

    #[test]
    fn project_for_cwd_subdirectory() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-test-myproject");
        fs::create_dir_all(&project_dir).unwrap();
        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let result = provider.project_for_cwd("/Users/test/myproject/src/lib");
        assert_eq!(result, Some("/Users/test/myproject".to_string()));
    }

    #[test]
    fn project_for_cwd_no_match() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("-Users-test-myproject");
        fs::create_dir_all(&project_dir).unwrap();
        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());
        let result = provider.project_for_cwd("/Users/test/other");
        assert_eq!(result, None);
    }

    #[test]
    fn discover_files_includes_subagents() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("-Users-alice-myproject");
        let sess = "abc12345-0000-0000-0000-000000000000";
        let sub = proj.join(sess).join("subagents");
        fs::create_dir_all(&sub).unwrap();
        fs::write(proj.join(format!("{sess}.jsonl")), "{}").unwrap();
        fs::write(sub.join("agent-x.jsonl"), "{}").unwrap();
        fs::write(sub.join("journal.jsonl"), "{}").unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());

        // No filter: top-level + subagent, but not the journal ledger.
        let all = provider
            .discover_files(&QueryScope::new(None, None, None))
            .unwrap();
        assert_eq!(
            all.len(),
            2,
            "session file + subagent file, journal excluded"
        );

        // Session filter matches the parent session dir for the nested file.
        let scoped = provider
            .discover_files(&QueryScope::new(None, Some("abc12345".to_string()), None))
            .unwrap();
        assert_eq!(scoped.len(), 2);
    }

    #[test]
    fn discover_files_unions_across_sources() {
        use crate::source::Source;
        let tmp = TempDir::new().unwrap();
        let main_dir = tmp.path().join("main").join("projects");
        let env_dir = tmp.path().join("cenv").join("pinwheel").join("projects");
        let main_proj = main_dir.join("-Users-josh-a");
        let env_proj = env_dir.join("-Users-josh-b");
        fs::create_dir_all(&main_proj).unwrap();
        fs::create_dir_all(&env_proj).unwrap();
        fs::write(
            main_proj.join("11111111-0000-0000-0000-000000000000.jsonl"),
            "{}",
        )
        .unwrap();
        fs::write(
            env_proj.join("22222222-0000-0000-0000-000000000000.jsonl"),
            "{}",
        )
        .unwrap();

        let provider = ClaudeProvider::with_sources(vec![
            Source {
                name: "main".into(),
                projects_dir: main_dir.clone(),
            },
            Source {
                name: "pinwheel".into(),
                projects_dir: env_dir.clone(),
            },
        ]);

        let files = provider
            .discover_files(&QueryScope::new(None, None, None))
            .unwrap();
        assert_eq!(files.len(), 2);

        // source_for_file maps a path back to its source name
        let f = files
            .iter()
            .find(|p| p.to_string_lossy().contains("pinwheel"))
            .unwrap();
        assert_eq!(provider.source_for_file(f).as_deref(), Some("pinwheel"));
    }

    #[test]
    fn source_scope_filters_to_one_source() {
        use crate::source::Source;
        let tmp = TempDir::new().unwrap();
        let main_dir = tmp.path().join("main").join("projects");
        let env_dir = tmp.path().join("cenv").join("pinwheel").join("projects");
        fs::create_dir_all(main_dir.join("-a")).unwrap();
        fs::create_dir_all(env_dir.join("-b")).unwrap();
        fs::write(main_dir.join("-a").join("s.jsonl"), "{}").unwrap();
        fs::write(env_dir.join("-b").join("s.jsonl"), "{}").unwrap();

        let provider = ClaudeProvider::with_sources(vec![
            Source {
                name: "main".into(),
                projects_dir: main_dir,
            },
            Source {
                name: "pinwheel".into(),
                projects_dir: env_dir,
            },
        ]);
        let scope = QueryScope::new(None, None, None).with_source(Some("pinwheel".into()));
        let files = provider.discover_files(&scope).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].to_string_lossy().contains("pinwheel"));
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
