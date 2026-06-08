use std::path::{Path, PathBuf};

/// A named transcript root that cq indexes. `name` is how the user refers to it
/// (`main`, `pinwheel`, ...); `projects_dir` is the directory containing the
/// encoded per-project session dirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub projects_dir: PathBuf,
}

/// The default "main" source: `CQ_PROJECTS_DIR` if set, else `~/.claude/projects`.
pub fn main_source() -> anyhow::Result<Source> {
    use anyhow::Context;
    let dir = if let Ok(dir) = std::env::var("CQ_PROJECTS_DIR") {
        PathBuf::from(dir)
    } else {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        home.join(".claude").join("projects")
    };
    Ok(Source { name: "main".to_string(), projects_dir: dir })
}

/// cenv sources: one per `<cenv base>/<name>/projects` that exists on disk.
/// Base is `$CENV_BASE` if set, else `~/.local/share/cenv`. Returns empty when
/// the base does not exist. Sorted by name for deterministic ordering.
pub fn discover_cenv_sources() -> Vec<Source> {
    let base = match std::env::var("CENV_BASE") {
        Ok(b) => PathBuf::from(b),
        Err(_) => match dirs::home_dir() {
            Some(h) => h.join(".local").join("share").join("cenv"),
            None => return Vec::new(),
        },
    };
    discover_cenv_sources_in(&base)
}

/// Testable core: scan `base/*/projects`. Only descends one level (the env dir),
/// never into the env's `plugins/` cache.
pub fn discover_cenv_sources_in(base: &Path) -> Vec<Source> {
    let mut sources = Vec::new();
    let Ok(entries) = std::fs::read_dir(base) else {
        return sources;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let projects_dir = entry.path().join("projects");
        if projects_dir.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            sources.push(Source { name, projects_dir });
        }
    }
    sources.sort_by(|a, b| a.name.cmp(&b.name));
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn discovers_only_envs_with_projects_dir() {
        let base = TempDir::new().unwrap();
        // env "alpha" has a projects/ dir
        fs::create_dir_all(base.path().join("alpha").join("projects")).unwrap();
        // env "beta" has only a plugins cache, no projects/
        fs::create_dir_all(base.path().join("beta").join("plugins")).unwrap();
        // a stray file at the base is ignored
        fs::write(base.path().join("notadir"), "x").unwrap();

        let sources = discover_cenv_sources_in(base.path());
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "alpha");
        assert!(sources[0].projects_dir.ends_with("alpha/projects"));
    }

    #[test]
    fn missing_base_returns_empty() {
        let sources = discover_cenv_sources_in(Path::new("/no/such/cenv/base"));
        assert!(sources.is_empty());
    }
}
