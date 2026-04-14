use std::path::PathBuf;
use anyhow::Result;
use duckdb::Connection;
use crate::scope::QueryScope;

pub trait TranscriptProvider {
    fn name(&self) -> &str;
    fn discover_files(&self, scope: &QueryScope) -> Result<Vec<PathBuf>>;
    fn register_views(&self, conn: &Connection, files: &[PathBuf]) -> Result<()>;
    fn list_projects(&self) -> Result<Vec<ProjectInfo>>;
}

#[derive(Debug)]
pub struct ProjectInfo {
    pub encoded_name: String,
    pub decoded_path: String,
    pub file_count: usize,
}
