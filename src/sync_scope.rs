use std::path::PathBuf;

/// Controls which project directories the indexer checks.
#[derive(Debug, Clone)]
pub enum SyncScope {
    /// Scan all project directories. Default for unscoped queries.
    All,
    /// Scan specific project directories only.
    Projects(Vec<PathBuf>),
    /// Check a single specific file only.
    File(PathBuf),
}
